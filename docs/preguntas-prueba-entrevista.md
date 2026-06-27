# Preguntas de prueba — `POST /interviews/:id/ai/assistant-relay`

Batería de **10 preguntas** (5 técnicas + 5 blandas) para evaluar el pipeline **detector → opener → deepener** en entrevistas de trabajo.

**Qué medir en cada prueba**

| Dimensión | Dónde mirar |
|-----------|-------------|
| **Detección** | Evento SSE `question`: ¿extrajo la pregunta correcta? |
| **Calidad opener** | 2 frases naturales, sin tono de manual, cierra en `—` si es técnica |
| **Calidad deepener** | Continúa sin corte, tono humano, texto en negrita (`**...**`), sin meta-comentarios |
| **Rapidez** | TTFT en logs + `openerTokens` / `deepenerTokens` / `totalTokens` en `metadata` |

**Body base** (ajusta `INTERVIEW_ID`, host y JWT según tu `.env`):

```bash
export BASE="http://127.0.0.1:6400"
export INTERVIEW_ID=200

curl -N -sS -X POST "${BASE}/interviews/${INTERVIEW_ID}/ai/assistant-relay" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "values": {
      "jobPosition": "Backend Developer",
      "regionalism": "Neutro",
      "responseLanguage": "es",
      "profileMinimal": "Backend con 5 años en APIs REST, microservicios y PostgreSQL.",
      "lastJobs": "Desarrollador backend en fintech",
      "roleKeywords": ["Go", "Kafka", "PostgreSQL", "DDD", "testing"]
    },
    "messages": [
      {"role": "user", "content": "<TRANSCRIPCION_AQUI>"}
    ]
  }'
```

> Con `RELAY_SKIP_JWT=1` puedes omitir el header `Authorization`.

---

## Técnicas (1–5)

### 1. Definición directa — fácil

**Transcripción simulada**
```
eh bueno cuéntame este... ¿qué es eso del ddd en microservicios no?
```

**Pregunta esperada (detector)**
```
¿Qué es DDD en microservicios?
```

**Qué evaluar**
- Opener: respuesta concreta en 2 frases, sin definición de libro.
- Deepener: ejemplo vivido (bounded context, lenguaje ubicuo, etc.) sin repetir el opener.

---

### 2. Comparación — media

**Transcripción simulada**
```
a ver dime las diferencias entre pruebas de rendimiento, pruebas de carga y pruebas de estrés
```

**Pregunta esperada**
```
Dime las diferencias entre pruebas de rendimiento, pruebas de carga y pruebas de estrés.
```

**Qué evaluar**
- Imperativo sin signos de interrogación: el detector debe captarla igual.
- Deepener: distingue los tres tipos con precisión, no mezcla conceptos.

---

### 3. Troubleshooting — media

**Transcripción simulada**
```
si encuentras un bug en produccion como lo priorizas o sea que haces primero
```

**Pregunta esperada**
```
Si encuentras un bug en producción, ¿cómo lo priorizas?
```

**Qué evaluar**
- Opener: menciona impacto, severidad o usuarios afectados.
- Deepener: proceso concreto (reproducir, rollback, hotfix, postmortem) sin sonar a checklist rígido.

---

### 4. Diseño / arquitectura — difícil

**Transcripción simulada**
```
imagina que tienes que diseñar un sistema de pagos que procese diez mil transacciones por segundo como lo estructurarias que componentes usarias y por que
```

**Pregunta esperada**
```
¿Cómo estructurarías un sistema de pagos que procese 10.000 transacciones por segundo?
```

**Qué evaluar**
- Pregunta larga y contextual: el detector debe quedarse con el núcleo.
- Deepener: menciona colas, idempotencia, particionado o consistencia eventual si encaja.

---

### 5. Imperativo con ruido — difícil (caso real)

**Transcripción simulada**
```
cierro cierro no? si encuentras un bug como lo priorizas? si testing bien mucha pues te falta a ver cierralo ahi a ver dame un ejemplo de login con gherkin y bdd
```

**Pregunta esperada**
```
Dame un ejemplo de login con Gherkin y BDD.
```

**Qué evaluar**
- Ruido de fondo + varias preguntas: debe tomar **solo la última**.
- Deepener: ejemplo concreto de escenario Gherkin, no teoría genérica de BDD.

---

## Blandas (6–10)

### 6. Fortaleza personal — fácil

**Transcripción simulada**
```
ya hablamos de tu stack perfecto ahora me interesa lo blando o sea cual dirías que es tu mayor fortaleza como desarrollador
```

**Pregunta esperada**
```
¿Cuál es tu mayor fortaleza como desarrollador?
```

**Qué evaluar**
- Opener: primera persona, seguro, sin "me apasiona".
- Deepener: ejemplo breve que demuestre la fortaleza, tono conversacional.

---

### 7. Conflicto en equipo — media

**Transcripción simulada**
```
cuéntame de una vez que tuviste un conflicto con alguien del equipo y como lo resolviste
```

**Pregunta esperada**
```
¿Cuéntame de una vez que tuviste un conflicto con alguien del equipo y cómo lo resolviste?
```

**Qué evaluar**
- Sin signos de interrogación explícitos.
- Deepener: situación concreta, acción tomada, resultado — sin moraleja final.

---

### 8. Proyecto que salió mal — media

**Transcripción simulada**
```
hablame de un proyecto que se te fue de las manos que paso y que aprendiste
```

**Pregunta esperada**
```
Háblame de un proyecto que se te fue de las manos, qué pasó y qué aprendiste.
```

**Qué evaluar**
- Opener: admite el problema sin victimizarse.
- Deepener: aprendizaje específico, no frases vacías tipo "fue muy gratificante".

---

### 9. Motivación / fit — media

**Transcripción simulada**
```
por que quieres trabajar aqui y por que deberiamos elegirte a ti sobre otros candidatos
```

**Pregunta esperada**
```
¿Por qué quieres trabajar aquí y por qué deberíamos elegirte a ti?
```

**Qué evaluar**
- Pregunta compuesta: el detector puede devolver una u otra parte; lo importante es que sea coherente.
- Respuesta: específica al rol backend, no genérica de "soy proactivo".

---

### 10. Liderazgo / comunicación — difícil

**Transcripción simulada**
```
supongamos que el product owner te pide una feature imposible para el deadline que le dices y como manejas esa conversacion
```

**Pregunta esperada**
```
¿Qué le dices al product owner si te pide una feature imposible para el deadline?
```

**Qué evaluar**
- Escenario situacional largo.
- Deepener: negociación concreta (alcance, MVP, trade-offs), tono profesional y humano.

---

## Checklist rápido por prueba

Marca ✅ / ❌ después de cada curl:

```
[ ] question.intelligible = true
[ ] question coincide con la esperada (o equivalente)
[ ] Opener: exactamente ~2 frases, natural, sin etiquetas
[ ] Hay espacio entre opener y deepener (no pegados)
[ ] Deepener: continúa sin "continuando...", "claro", "necesito..."
[ ] Deepener: sin tono IA / TED / LinkedIn
[ ] metadata.totalTokens razonable (< 4000 en condiciones normales)
[ ] Tiempo hasta primer chunk aceptable (< 2–3 s en local)
```

---

## Orden sugerido de ejecución

1. **1, 6** — calentamiento (fáciles, validan flujo básico).
2. **2, 3, 7** — casos medios variados.
3. **5** — estrés del detector (ruido + última pregunta).
4. **4, 10** — preguntas largas (calidad del deepener bajo carga).
5. **8, 9** — blandas con riesgo de respuestas genéricas.

---

## Respuesta SSE esperada (estructura)

```text
event: question
data: {"question":"...","intelligible":true}

data: ["Fíjate,"]
data: [" DDD"]
...
data: [" —"]

data: [" "]
data: ["**"]
data: ["porque"]
...
data: ["**"]

event: metadata
data: {"openerTokens":...,"deepenerTokens":...,"totalTokens":...}

data: [DONE]
```
