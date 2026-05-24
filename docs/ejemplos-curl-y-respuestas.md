# Ejemplos: `curl` y respuestas del relay

Endpoint: **`POST /interviews/:id/ai/assistant-relay`**

- **`:id`**: debe coincidir con el claim **`interviewId`** del JWT.
- **Autenticación**: cabecera `Authorization: Bearer <JWT>` (HS256, misma clave que `RELAY_JWT_SECRET`).
- **Cuerpo**: JSON con **`messages`** (array no vacío de mensajes estilo chat).
- Opcional: **`maxOutputTokens`** / **`max_output_tokens`** (u32): límite de tokens de respuesta pedido por el cliente; el servidor lo recorta al techo `INTERVIEW_AGENT_MAX_TOKENS` y como mínimo 64.

Claims típicos del JWT: `sub` (id de usuario), `interviewId`, `exp`; opcional `iat` (si existe, `exp - iat` no puede superar 5 minutos).

---

## Ejemplo con `curl`

Ajusta **host**, **puerto** (`PORT` en `.env`; si no existe, el servidor usa **3001**), **ID de entrevista** y el **JWT**.

```bash
export JWT="eyJhbGciOiJIUzI1NiJ9..."   # token HS256 válido
export BASE="http://127.0.0.1:3070"   # o :3001 según tu configuración
export INTERVIEW_ID=999

curl -N -sS -X POST "${BASE}/interviews/${INTERVIEW_ID}/ai/assistant-relay" \
  -H "Authorization: Bearer ${JWT}" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "messages": [
      {"role": "system", "content": "Eres un entrevistador breve."},
      {"role": "user", "content": "¿Qué es Rust?"}
    ]
  }'
```

- **`-N`**: evita que el cliente bufferice la salida; así ves el SSE en tiempo real.

Para generar un JWT de prueba puedes usar [jwt.io](https://jwt.io) (algoritmo **HS256**) o emitirlo desde tu backend (p. ej. Nest).

---

## Respuesta correcta (200): SSE

Cabeceras habituales (entre otras):

- `Content-Type: text/event-stream`
- `Cache-Control: no-cache, no-transform`
- `Connection: keep-alive`
- `X-Accel-Buffering: no`

Cuerpo: stream **SSE**. Cada evento lleva una línea `data:`; el relay reenvía fragmentos al estilo OpenAI (`choices` / `delta`) y cierra con `[DONE]`.

Ejemplo ilustrativo (los trozos exactos dependen de OpenAI):

```text
data: {"choices":[{"index":0,"delta":{"content":"Rust"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"content":" es"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"content":" un lenguaje..."},"finish_reason":"stop"}]}

data: [DONE]

```

---

## Respuestas de error (JSON)

Los errores devuelven **JSON** con `message` y `error` (el detalle humano suele ir en `message`).

### 401 — Bearer ausente o JWT inválido

```http
HTTP/1.1 401 Unauthorized
Content-Type: application/json
```

```json
{
  "message": "Authorization Bearer faltante o inválido",
  "error": "Authorization Bearer faltante o inválido"
}
```

(Con token mal firmado o expirado, el `message` puede ser el texto del error de decodificación.)

### 403 — Entrevista del token distinta a la ruta

```json
{
  "message": "entrevista del token no coincide con la ruta",
  "error": "entrevista del token no coincide con la ruta"
}
```

### 400 — `messages` vacío

```json
{
  "message": "messages es obligatorio y no puede estar vacío",
  "error": "messages es obligatorio y no puede estar vacío"
}
```

### 429 — Rate limit (Redis)

```json
{
  "message": "Máximo 10 peticiones por ventana (usuario + entrevista)",
  "error": "rate limit: máximo 10 peticiones / ventana por usuario y entrevista"
}
```

El número (**10** en el ejemplo) lo define **`RATE_LIMIT_MAX`** en el entorno.

### 503 — Redis no disponible al aplicar el límite

```json
{
  "message": "servicio de límites (Redis) no disponible",
  "error": "servicio de límites (Redis) no disponible"
}
```

### 502 — Error al llamar al proveedor de IA

El texto varía según el fallo (red, API key, respuesta de OpenAI, etc.):

```json
{
  "message": "OpenAI API error: ...",
  "error": "AI provider error: OpenAI API error: ..."
}
```

---

## Referencia rápida

| Código | Situación |
|--------|-----------|
| 200 | Stream SSE correcto |
| 400 | Cuerpo inválido (`messages` vacío) |
| 401 | Auth / JWT / TTL del token |
| 403 | `interviewId` ≠ id de la URL |
| 429 | Rate limit |
| 502 | Fallo proveedor IA |
| 503 | Redis del rate limit |
