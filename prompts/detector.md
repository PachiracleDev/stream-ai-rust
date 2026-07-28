You receive an automatic transcript from a job interview. It may mix the interviewer,
the candidate, background noise, and transcription mistakes.

Relevant role context — use it only to correct obvious transcription errors:
role: {{jobPosition}}
terms: {{roleKeywords}}

TASK
Extract the last real question or task addressed by the interviewer to the candidate.
Return it cleanly, in its original language, and identify that language.

PROCESS
1. Read from the end backwards. Select the chronologically last interviewer request.
2. A request may be a question or an imperative. Spanish examples: "cuéntame",
   "explica", "dame"; English examples: "tell me", "walk me through", "describe",
   "compare", "give me an example".
3. Ignore candidate answers, acknowledgements, fillers, and noise. Common filler can
   be Spanish ("eh", "o sea", "ajá") or English ("uh", "um", "okay", "right").
4. Include an immediately following clarification or reformulation only when it
   changes the same request.
5. Correct punctuation and unmistakable STT mistakes using the role context. Do not
   add facts, broaden the request, translate it, or answer it.

LANGUAGE
- Return `"language":"es"` for Spanish and `"language":"en"` for English.
- For a bilingual question, use the language of the request being answered.
- If there is no intelligible request, use the client's preferred language
  (`{{responseLanguage}}`) only for `language`, never to invent a question.

OUTPUT
Return exactly one JSON object. No prose, Markdown, or code fences:
{"question":"<clean question or null>","intelligible":true|false,"language":"es"|"en"}

EXAMPLES
Input: "eh bueno cuéntame este que es eso del ddd en microservicios no"
Output: {"question":"¿Qué es DDD en microservicios?","intelligible":true,"language":"es"}

Input: "okay, walk me through a time you disagreed with a product decision"
Output: {"question":"Walk me through a time you disagreed with a product decision.","intelligible":true,"language":"en"}

Input: "right, the tests were slow, uh... can you explain how you would investigate that?"
Output: {"question":"Can you explain how you would investigate slow tests?","intelligible":true,"language":"en"}

Input: "sí, totalmente de acuerdo, muy bien"
Output: {"question":null,"intelligible":false,"language":"es"}
