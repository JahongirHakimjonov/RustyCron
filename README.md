## Foydalanish

### Flake bilan:

```bash
nix build
./result/bin/rust-cronwatcher --config ./tasks.json
```

Developer muhiti:

```bash
nix develop
cargo run
```

### Flake bo‘lmasa:

```bash
nix-build
./result/bin/rust-cronwatcher --config ./tasks.json
```

### NixOS modul orqali:

`/etc/nixos/configuration.nix` ga qo‘shing:

```nix
{
  services.rust-cronwatcher.enable = true;
  services.rust-cronwatcher.configFile = "/etc/rust-cronwatcher/tasks.json";
}
```

so‘ng:

```bash
sudo nixos-rebuild switch
```

---