# Instalar Rust en Linux

Usa **rustup** (instalador oficial). En la terminal:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Cuando pregunte, elige la opción por defecto (**1**) y termina el asistente.

Carga `cargo` en la sesión actual (o abre una terminal nueva):

```bash
source "$HOME/.cargo/env"
```

Comprueba:

```bash
rustc --version
cargo --version
```

Para que quede en **todas** las terminales, rustup suele añadir una línea a `~/.profile` o `~/.bashrc`. Si `cargo` solo funciona tras `source`, añade a `~/.bashrc`:

```bash
. "$HOME/.cargo/env"
```

Luego en el proyecto:

```bash
make run
```

**Desinstalar** (si lo necesitas): `rustup self uninstall`
