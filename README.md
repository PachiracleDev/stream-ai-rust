# Interview Relay (Rust)

Proxy HTTP para entrevistas con IA: **SSE en streaming** hacia OpenAI o Claude, validación **JWT**, **rate limiting** con **Redis** y configuración por variables de entorno.

## Características

- Proveedores **OpenAI** y **Claude** (`AI_PROVIDER`)
- Respuesta **SSE** (`text/event-stream`), keep-alive y headers adecuados para no bufferizar
- **JWT** HS256 (`RELAY_JWT_SECRET`, claims `sub`, `interviewId`, `exp`, `iat` opcional; TTL máx. 5 min si hay `iat`)
- **Rate limit** en Redis por `sub` + `interviewId` (`RATE_LIMIT_MAX`, `RATE_LIMIT_WINDOW_SECS`)
- **CORS** permisivo por defecto (ajustar en producción si hace falta)
- Apagado limpio con `Ctrl+C`

## Requisitos

- **Rust** (toolchain estable). Instalación en Linux: [docs/instalar-rust-linux.md](docs/instalar-rust-linux.md)
- **Redis** ya corriendo y alcanzable (este repo no lo levanta). Define `REDIS_URL` en `.env` (p. ej. `redis://127.0.0.1:6379`)
- Clave de API según proveedor: `OPENAI_API_KEY` y/o `ANTHROPIC_API_KEY`

## Configuración

```bash
cp .env.example .env
```

Edita al menos: `RELAY_JWT_SECRET`, `REDIS_URL`, `OPENAI_API_KEY` (o claves Claude si usas ese proveedor). El resto de variables está documentado en [.env.example](.env.example). Comportamiento por defecto del servidor: `PORT` / `LISTEN` o **3001** si no se definen.

## Ejecutar

Desde la raíz del repo:

```bash
make run
```

Equivale a comprobar `.env` y ejecutar `cargo run` (con `PATH` típico de rustup). Para producción en la misma máquina suele usarse el binario release:

```bash
cargo build --release
./target/release/interview-relay-sim
```

La aplicación carga `.env` al arrancar (dotenvy).

## Probar el endpoint

Ruta: `POST /interviews/:id/ai/assistant-relay`  
Cabecera: `Authorization: Bearer <JWT>`  
Cuerpo JSON: `{ "messages": [ ... ] }` (no vacío). El `interviewId` del JWT debe coincidir con `:id` de la URL.

Ejemplo (sustituye `JWT` por un token válido firmado con `RELAY_JWT_SECRET` y el `interviewId` adecuado; el puerto debe coincidir con tu `PORT`):

```bash
curl -N -X POST "http://localhost:3001/interviews/999/ai/assistant-relay" \
  -H "Authorization: Bearer $JWT" \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "system", "content": "Eres un entrevistador."},
      {"role": "user", "content": "Hola"}
    ]
  }'
```

`-N` desactiva el buffering del cliente para ver el SSE al momento.

Más ejemplos de `curl`, cuerpo SSE y respuestas JSON de error: **[docs/ejemplos-curl-y-respuestas.md](docs/ejemplos-curl-y-respuestas.md)**.

## Cambiar proveedor o parámetros del modelo

Valores típicos (ver `.env.example`):

```bash
export AI_PROVIDER=claude
export ANTHROPIC_API_KEY=sk-ant-...
export INTERVIEW_AGENT_MODEL=claude-3-5-sonnet-20241022
```

Ajuste de contexto / límites: `INTERVIEW_AGENT_MAX_TOKENS`, `INTERVIEW_AGENT_TEMPERATURE`, `INTERVIEW_AGENT_MAX_HISTORY`, `RATE_LIMIT_MAX`, etc.

## Logs

```bash
RUST_LOG=interview_relay_sim=debug cargo run
```

## Despliegue en Linux (systemd)

Patrón habitual: compilar release, instalar binario y usar un fichero de entorno **fuera** del repo.

Ejemplo de unidad (adapta rutas y usuario):

```ini
[Unit]
Description=Interview relay
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/interview-relay
EnvironmentFile=/opt/interview-relay/.env
ExecStart=/opt/interview-relay/interview-relay-sim
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Genera el binario con `cargo build --release` y copia `target/release/interview-relay-sim` donde corresponda. Asegura que Redis y las API keys configuradas en `EnvironmentFile` sean correctas.

## Solución de problemas

| Problema | Qué revisar |
|----------|-------------|
| No encuentra `cargo` | [docs/instalar-rust-linux.md](docs/instalar-rust-linux.md) y `source ~/.cargo/env` |
| Error de Redis / rate limit | `REDIS_URL`, firewall, que el servicio Redis esté activo |
| `OPENAI_API_KEY` / Claude | Variables en `.env` o en el `EnvironmentFile` de systemd |
| JWT inválido o 403 | Misma clave que `RELAY_JWT_SECRET`, `interviewId` igual al de la URL, `exp` vigente |
| 429 rate limit | `RATE_LIMIT_MAX` / `RATE_LIMIT_WINDOW_SECS` o esperar la ventana |
| `openssl-sys` / `pkg-config` al compilar | El cliente HTTP usa **rustls**; no hace falta `libssl-dev`. Haz `cargo clean` y vuelve a `cargo build` por si quedó caché de un build antiguo |

## Desarrollo y tests

```bash
cargo test
cargo clippy
```

Detalles de claims JWT y script Lua del rate limit: comentarios en [src/main.rs](src/main.rs).

## Licencia

MIT
