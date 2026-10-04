# Contrato de endpoints — stream-ai-rust

Base URL: `http://{HOST}:{PORT}` (default `PORT=6400`).

Todos los endpoints bajo `/interviews/{interviewId}/ai/*` requieren:

```
Authorization: Bearer {relayJwt}
Content-Type: application/json
```

**JWT (HS256)**: claims `sub` (userId), `interviewId` (debe coincidir con el `:id` de la ruta), `exp`, opcional `iat` (si existe, `exp - iat` ≤ 5 min).

**Errores comunes a todos** (JSON):

| HTTP | Caso | Body ejemplo |
|------|------|--------------|
| 400 | Body inválido / campo vacío | `{"message":"...","error":"..."}` |
| 401 | Bearer ausente o JWT inválido/expirado | `{"message":"Authorization Bearer faltante o inválido","error":"..."}` |
| 403 | `interviewId` del token ≠ `:id` de la ruta | `{"message":"entrevista del token no coincide con la ruta","error":"..."}` |
| 422 | JSON no deserializa (falta campo, valor inválido) | texto plano con el campo faltante |
| 429 | Rate limit | `{"message":"Máximo N peticiones por ventana...","error":"..."}` |
| 502 | Fallo del proveedor IA | `{"message":"...","error":"AI provider error: ..."}` |

---

## 1. `POST /interviews/{interviewId}/ai/question-detect`

Detecta si el último fragmento STT del entrevistador contiene una **pregunta que el candidato aún no ha respondido**. Responde **JSON** (no SSE).

El servidor mantiene memoria por sesión (`userId` + `sessionId`): últimos 8 párrafos STT, última pregunta pendiente y preguntas ya respondidas. El front solo envía el fragmento nuevo.

### Request

```json
{
  "sessionId": "273",
  "text": "Tell me about a project that failed or didn't turn out as you expected.",
  "kind": "tecnica",
  "values": {
    "jobPosition": "Full stack developer",
    "regionalism": "Peruano",
    "responseLanguage": "es",
    "profileMinimal": "...",
    "lastJobs": "...",
    "roleKeywords": ["React", "Node.js"]
  }
}
```

| Campo | Req | Descripción |
|-------|-----|-------------|
| `sessionId` | ✅ | ID de sesión (string). Agrupa la memoria de la entrevista. |
| `text` | ✅ | Fragmento STT nuevo del entrevistador. |
| `kind` | ➖ | `"recursos-humanos"` \| `"tecnica"` (default `tecnica`). |
| `values` | ➖ | Contexto del candidato. Si falta, se usan defaults genéricos. |

### Response 200 — pregunta nueva

```json
{
  "shouldRespond": true,
  "question": "Tell me about a project that failed or didn't turn out as you expected.",
  "intelligible": true
}
```

### Response 200 — continuación de pregunta pendiente

Si el entrevistador hace una pausa y complementa su pregunta anterior, se devuelve **combinada**:

```json
{
  "shouldRespond": true,
  "question": "Tell me about a project that failed or didn't turn out as you expected. What happened, and what did you learn?",
  "intelligible": true
}
```

### Response 200 — follow-up corto / indirecto

Fragmentos como `¿Cómo?`, `How so?`, `y eso por qué?` se **expanden** usando el contexto de la sesión:

```json
{
  "shouldRespond": true,
  "question": "¿Por qué React usa el virtual DOM?",
  "intelligible": true
}
```

### Response 200 — escenario + pregunta

Cuando el entrevistador plantea un escenario y luego pregunta sobre él, `question` trae **los dos juntos**, en el mismo request o en requests distintos:

```json
{
  "shouldRespond": true,
  "question": "Tienes una API Java que hace 5 llamadas HTTP externas en secuencia y tarda 4 segundos. ¿Qué alternativas analizarías para reducir la latencia?",
  "intelligible": true
}
```

Si un fragmento **solo** plantea el escenario ("Tienes una API Java que…", "Imagina que tu equipo…") y aún no pregunta nada, responde `{ "shouldRespond": false }` y el escenario queda guardado en la sesión. Se antepone a la siguiente pregunta. Un escenario con tarea implícita ("Imagina que tienes que diseñar un sistema de pagos") sí se responde de inmediato.

Si el STT reenvía una pregunta ya detectada seguida de otra **de otro tema**, solo se devuelve la nueva.

### Response 200 — nada que responder

Repetición, pregunta ya respondida, saludo, relleno o escenario sin pregunta todavía:

```json
{ "shouldRespond": false }
```

o, cuando el fragmento tenía una pregunta pero ya fue respondida:

```json
{ "shouldRespond": false, "question": "", "intelligible": false }
```

### Uso recomendado

- Llamar con cada fragmento STT nuevo del entrevistador.
- Si `shouldRespond: true` → enviar `question` como último `messages[].content` (role `user`) a `assistant-relay`.
- Cada fragmento lo evalúa el modelo (cualquier idioma o puesto), ~300–550 ms.

---

## 2. `POST /interviews/{interviewId}/ai/assistant-relay`

Genera la **respuesta del candidato** a una pregunta ya extraída. Responde **SSE** (`Accept: text/event-stream`).

El último `messages[]` con `role: "user"` ES la pregunta final (limpia, viene de `question-detect`). Este endpoint **no detecta preguntas ni emite `event: question`**.

### Request

```json
{
  "kind": "tecnica",
  "values": {
    "jobPosition": "Full stack developer",
    "regionalism": "Peruano",
    "responseLanguage": "es",
    "profileMinimal": "5 años en sistemas distribuidos...",
    "lastJobs": "Backend senior en fintech...",
    "roleKeywords": ["React", "Node.js", "microservices"]
  },
  "messages": [
    { "role": "user", "content": "¿Qué es el virtual DOM?" },
    { "role": "assistant", "content": "Mira, el virtual DOM es..." },
    { "role": "user", "content": "¿Y por qué React lo usa?" }
  ]
}
```

| Campo | Req | Descripción |
|-------|-----|-------------|
| `kind` | ✅ | `"recursos-humanos"` \| `"tecnica"`. **Obligatorio.** Se evalúa por request: si el usuario cambia el tipo mid-conversación, la siguiente llamada ya responde en el modo nuevo (el historial previo no arrastra el estilo viejo). |
| `values` | ✅ | Contexto del candidato. |
| `values.jobPosition` | ✅ | Puesto. |
| `values.regionalism` | ✅ | Región/acento (p. ej. `"Peruano"`, `"es-MX"`). |
| `values.responseLanguage` | ✅ | Idioma de respuesta: `"es"`, `"en"`, `"español"`, `"english"`... La pregunta actual manda sobre este default. |
| `values.profileMinimal` | ➖ | Perfil corto. |
| `values.lastJobs` | ➖ | Experiencia reciente. |
| `values.roleKeywords` | ➖ | Array o string de términos del puesto. |
| `messages` | ✅ | Historial (`user`/`assistant`) + última pregunta como último `user`. Vacío o último user sin contenido → 400. |

### Imagen (image solver)

Si algún mensaje trae `imageUrl` (HTTPS de host permitido o `data:image/png;base64,...`), el endpoint cambia automáticamente a modo imagen y resuelve lo que aparece en ella:

```json
{
  "kind": "tecnica",
  "values": { "jobPosition": "...", "regionalism": "...", "responseLanguage": "es" },
  "messages": [
    {
      "role": "user",
      "content": "Resuelve el ejercicio de la imagen paso a paso.",
      "imageUrl": "data:image/png;base64,iVBORw0KGgo..."
    }
  ]
}
```

### Response 200 — SSE

```
data: ["Mira,"]
data: [" la"]
data: [" diferencia"]
...
event: metadata
data: {"openerTokens":0,"deepenerTokens":487,"totalTokens":1234,"elapsedMs":812}

data: [DONE]
```

| Evento | Descripción |
|--------|-------------|
| `data: ["..."]` | Fragmento de texto de la respuesta. Concatenar todos los strings. |
| `event: metadata` | Tokens: `deepenerTokens` y `totalTokens` son los de esta respuesta. `openerTokens` queda en `0` por compatibilidad con clientes viejos (ya no hay opener). |
| `data: [DONE]` | Fin del stream. |

**Nunca** emite `event: question` ni JSON de detección.

### Errores específicos

| HTTP | Caso |
|------|------|
| 400 | `messages` vacío; último `user` sin contenido; imagen sin `imageUrl` válido |
| 422 | falta `kind` o `values`; `kind` inválido (p. ej. `"mixta"` ya no existe) |

---

## 3. `POST /interviews/{interviewId}/ai/translation-relay`

Traducción en vivo vía SSE.

### Request

```json
{
  "text": "¿Podrías contarme sobre tu experiencia liderando equipos?",
  "targetLanguage": "en",
  "sourceLanguage": "es"
}
```

| Campo | Req | Descripción |
|-------|-----|-------------|
| `text` | ✅ | 1–4000 caracteres. |
| `targetLanguage` | ✅ | `"es"` \| `"en"` \| `"pt"`. |
| `sourceLanguage` | ➖ | Idioma origen (pista). |

### Response 200 — SSE

```
data: {"token":"Could "}
data: {"token":"you tell me "}
...
event: metadata
data: {"model":"...","completionTokens":42,"totalTokens":128,"tokensPerSecond":85.3,"elapsedMs":612}

data: [DONE]
```

---

## 4. `POST /interviews/{interviewId}/ai/expand-response`

Extiende una respuesta ya dada (profundiza donde quedó). SSE. Rate limit propio: 1/min por usuario.

### Request

```json
{
  "question": "¿Qué es el virtual DOM?",
  "response": "Mira, el virtual DOM es una representación en memoria...",
  "values": {
    "jobPosition": "Full stack developer",
    "regionalism": "Peruano",
    "responseLanguage": "es"
  }
}
```

### Response 200 — SSE

```
data: ["porque"]
data: [" si"]
...
event: metadata
data: {"deepenerTokens":210,"totalTokens":210}

data: [DONE]
```

---

## 5. `GET /health`

Healthcheck sin auth. Response 200: `ok`.
