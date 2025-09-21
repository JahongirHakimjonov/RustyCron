use anyhow::{Context, Result};
use chrono::Utc;
use cron::Schedule;
use notify::{recommended_watcher, EventKind, RecursiveMode, Watcher};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{broadcast, Semaphore};
use tokio::time::sleep;
use tracing::{error, info, warn};

#[derive(Debug, Deserialize, Clone)]
struct Task {
    id: String,
    user: Option<String>,
    schedule: String,
    command: String,
    timeout_seconds: Option<u64>,
    concurrency_group: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TasksFile {
    tasks: Vec<Task>,
}

#[derive(Debug, Clone)]
struct Job {
    task: Task,
    schedule: Schedule,
}

fn load_tasks_file(path: &PathBuf) -> Result<TasksFile> {
    let s = fs::read_to_string(path).with_context(|| format!("read tasks file {:?}", path))?;
    let tf: TasksFile = serde_json::from_str(&s)?;
    Ok(tf)
}

fn duration_until(target: chrono::DateTime<chrono::Utc>) -> Duration {
    let now = Utc::now();
    if target <= now {
        Duration::from_secs(0)
    } else {
        let diff = target - now;
        let secs = diff.num_seconds() as u64;
        let nanos =
            diff.num_nanoseconds().unwrap_or(0) - (diff.num_seconds() * 1_000_000_000) as i64;
        let nanos = nanos.max(0) as u32;
        Duration::from_secs(secs) + Duration::from_nanos(nanos as u64)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let tasks_path = PathBuf::from("tasks.json");

    // Broadcast channel for reload
    let (reload_tx, _reload_rx) = broadcast::channel::<()>(16);

    // Global concurrency
    let global_concurrency = Arc::new(Semaphore::new(100));

    // Group semaphores map
    let group_map: Arc<tokio::sync::Mutex<HashMap<String, Arc<Semaphore>>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // initial load + spawn schedulers
    let jobs = load_and_parse_jobs(&tasks_path)?;
    let mut handles = Vec::new();

    for job in jobs {
        let reload_rx = reload_tx.subscribe();
        let global_concurrency = global_concurrency.clone();
        let group_map = group_map.clone();
        handles.push(tokio::spawn(run_job_scheduler(
            job,
            reload_rx,
            global_concurrency,
            group_map,
        )));
    }

    // notify watcher -> tokio channel
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel(10);

    // RecommendedWatcher (notify >= 8): используем recommended_watcher
    let mut watcher = recommended_watcher(move |res| match res {
        Ok(event) => {
            let _ = notify_tx.blocking_send(event);
        }
        Err(e) => error!("watch error: {:?}", e),
    })?;
    watcher.watch(&tasks_path, RecursiveMode::NonRecursive)?;

    info!("Watching {:?} for changes", tasks_path);

    loop {
        tokio::select! {
            Some(event) = notify_rx.recv() => {
                if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    info!("Detected tasks file change: {:?}", event);
                    let _ = reload_tx.send(());

                    // small debounce for writers
                    sleep(Duration::from_millis(200)).await;

                    match load_and_parse_jobs(&tasks_path) {
                        Ok(new_jobs) => {
                            info!("Loaded {} tasks", new_jobs.len());
                            for job in new_jobs {
                                let reload_rx = reload_tx.subscribe();
                                let global_concurrency = global_concurrency.clone();
                                let group_map = group_map.clone();
                                handles.push(tokio::spawn(run_job_scheduler(job, reload_rx, global_concurrency, group_map)));
                            }
                        }
                        Err(e) => {
                            error!("Failed to load tasks: {:?}", e);
                        }
                    }
                }
            }

            else => {
                break;
            }
        }
    }

    Ok(())
}

fn load_and_parse_jobs(path: &PathBuf) -> Result<Vec<Job>> {
    let tf = load_tasks_file(path)?;
    let mut jobs = Vec::new();
    for t in tf.tasks {
        match Schedule::from_str(&t.schedule) {
            Ok(schedule) => {
                jobs.push(Job { task: t, schedule });
            }
            Err(e) => {
                warn!(
                    "Invalid cron expression for task {}: {} -- skipping",
                    t.id, e
                );
            }
        }
    }
    Ok(jobs)
}

async fn run_job_scheduler(
    job: Job,
    mut reload_rx: broadcast::Receiver<()>,
    global_concurrency: Arc<Semaphore>,
    group_map: Arc<tokio::sync::Mutex<HashMap<String, Arc<Semaphore>>>>,
) {
    let task = job.task.clone();
    let schedule = job.schedule.clone();
    info!("Scheduler started for task {}", task.id);

    loop {
        let mut upcoming = schedule.upcoming(Utc);
        match upcoming.next() {
            Some(next_dt) => {
                let dur = duration_until(next_dt);
                info!("Task {} next run at {} (in {:?})", task.id, next_dt, dur);

                tokio::select! {
                    _ = sleep(dur) => {
                        // Acquire global permit (owned)
                        let permit = match global_concurrency.clone().acquire_owned().await {
                            Ok(p) => p,
                            Err(e) => {
                                error!("Failed to acquire global semaphore for {}: {:?}", task.id, e);
                                continue;
                            }
                        };

                        // Acquire group permit (owned) if any
                        let group_permit_opt = if let Some(gr) = &task.concurrency_group {
                            let sem = {
                                let mut map = group_map.lock().await;
                                map.entry(gr.clone())
                                    .or_insert_with(|| Arc::new(Semaphore::new(10)))
                                    .clone()
                            };
                            match sem.clone().acquire_owned().await {
                                Ok(gp) => Some(gp),
                                Err(e) => {
                                    error!("Failed to acquire group semaphore for {}: {:?}", task.id, e);
                                    // release global permit implicitly by dropping `permit` when we continue
                                    drop(permit);
                                    continue;
                                }
                            }
                        } else {
                            None
                        };

                        // move permits into spawned task (they're OwnedSemaphorePermit -> Send + 'static)
                        let t_clone = task.clone();
                        tokio::spawn(async move {
                            // keep permits alive in this scope so they hold concurrency limit
                            let _global_permit = permit;
                            let _group_permit = group_permit_opt;

                            if let Err(e) = execute_task(t_clone).await {
                                error!("Task execution error: {:?}", e);
                            }
                            // permits dropped here, releasing semaphores
                        });
                    }

                    _ = reload_rx.recv() => {
                        info!("Scheduler for task {} received reload signal, exiting scheduler loop", task.id);
                        break;
                    }
                }
            }
            None => {
                warn!("No upcoming schedule for task {}", task.id);
                tokio::select! {
                    _ = reload_rx.recv() => {
                        info!("Scheduler for task {} exiting (no upcoming & reload)", task.id);
                        break;
                    }
                    _ = sleep(Duration::from_secs(60 * 60)) => {
                        // re-check in an hour
                    }
                }
            }
        }
    }

    info!("Scheduler stopped for task {}", task.id);
}

async fn execute_task(task: Task) -> Result<()> {
    info!(
        "Executing task {}: {} --USER--> {}",
        task.id,
        task.command,
        task.user.as_deref().unwrap_or("default")
    );

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(task.command);

    if let Some(timeout_s) = task.timeout_seconds {
        // keep child alive while waiting
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn command for task {}", task.id))?;
        let fut = child.wait();
        let res = tokio::time::timeout(Duration::from_secs(timeout_s), fut).await;

        match res {
            Ok(Ok(status)) => {
                info!("Task {} finished with status {:?}", task.id, status);
            }
            Ok(Err(e)) => {
                error!("Task {} failed to run: {:?}", task.id, e);
            }
            Err(_) => {
                error!("Task {} timed out after {}s", task.id, timeout_s);
                // best-effort kill
                match child.kill().await {
                    Ok(_) => info!("Task {} killed after timeout", task.id),
                    Err(e) => error!("Failed to kill timed-out task {}: {:?}", task.id, e),
                }
            }
        }
    } else {
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn command for task {}", task.id))?;
        match child.wait().await {
            Ok(status) => info!("Task {} finished with status {:?}", task.id, status),
            Err(e) => error!("Task {} failed: {:?}", task.id, e),
        }
    }

    Ok(())
}
