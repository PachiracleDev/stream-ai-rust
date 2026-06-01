//! Convierte historial estilo OpenAI (texto + imageUrl) al formato Messages API de Anthropic.

use serde_json::{json, Value};

fn message_content_as_string(c: &Value) -> Result<String, String> {
    match c {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        Value::Array(parts) => {
            let mut s = String::new();
            for p in parts {
                let Some(o) = p.as_object() else { continue };
                if o.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = o.get("text").and_then(|x| x.as_str()) {
                        s.push_str(t);
                    }
                }
            }
            Ok(s)
        }
        _ => Err("content: se esperaba string, null o lista de bloques".into()),
    }
}

fn openai_part_to_anthropic_block(o: &serde_json::Map<String, Value>) -> Result<Value, String> {
    match o.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            let text = o.get("text").and_then(|x| x.as_str()).unwrap_or("");
            Ok(json!({ "type": "text", "text": text }))
        }
        Some("image_url") => {
            let url = o
                .get("image_url")
                .and_then(|iu| iu.get("url"))
                .and_then(|u| u.as_str())
                .ok_or_else(|| "image_url sin url".to_string())?;
            Ok(json!({
                "type": "image",
                "source": { "type": "url", "url": url }
            }))
        }
        Some(other) => Err(format!("bloque OpenAI no soportado para Claude: {other}")),
        None => Err("bloque sin type".into()),
    }
}

fn openai_content_to_anthropic_user(c: &Value) -> Result<Value, String> {
    match c {
        Value::String(s) => Ok(Value::String(s.clone())),
        Value::Null => Ok(Value::String(String::new())),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for p in parts {
                let o = p
                    .as_object()
                    .ok_or_else(|| "bloque de content inválido".to_string())?;
                blocks.push(openai_part_to_anthropic_block(o)?);
            }
            if blocks.is_empty() {
                Ok(Value::String(String::new()))
            } else if blocks.len() == 1 && blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
            {
                Ok(Value::String(
                    blocks[0]
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                ))
            } else {
                Ok(Value::Array(blocks))
            }
        }
        _ => Err("content inválido para mensaje user".into()),
    }
}

fn last_role_is(out: &[Value], role: &str) -> bool {
    out.last()
        .and_then(|m| m.get("role"))
        .and_then(|r| r.as_str())
        == Some(role)
}

fn user_content_is_empty(c: &Value) -> bool {
    match c {
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

fn push_user(out: &mut Vec<Value>, content: Value) {
    if user_content_is_empty(&content) {
        return;
    }
    if last_role_is(out, "user") {
        let last = out.last_mut().expect("non-empty");
        let obj = last.as_object_mut().expect("object");
        let prev = obj.get("content").cloned().unwrap_or(Value::Null);
        obj.insert("content".into(), merge_user_content(prev, content));
    } else {
        out.push(json!({ "role": "user", "content": content }));
    }
}

fn merge_user_content(a: Value, b: Value) -> Value {
    let mut blocks = blocks_from_user(a);
    blocks.extend(blocks_from_user(b));
    if blocks.is_empty() {
        return Value::String(String::new());
    }
    if blocks.len() == 1 && blocks[0].get("type").and_then(|t| t.as_str()) == Some("text") {
        return Value::String(
            blocks[0]
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        );
    }
    Value::Array(blocks)
}

fn blocks_from_user(v: Value) -> Vec<Value> {
    match v {
        Value::String(s) if s.is_empty() => vec![],
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        Value::Array(a) => a,
        Value::Null => vec![],
        _ => vec![json!({"type": "text", "text": v.to_string()})],
    }
}

fn push_assistant(out: &mut Vec<Value>, content: Value) {
    if user_content_is_empty(&content) {
        return;
    }
    if last_role_is(out, "assistant") {
        let last = out.last_mut().expect("non-empty");
        let obj = last.as_object_mut().expect("object");
        let prev = obj.get("content").cloned().unwrap_or(Value::Null);
        let merged = match (prev, content) {
            (Value::String(mut a), Value::String(b)) => {
                a.push_str(&b);
                Value::String(a)
            }
            (a, b) => merge_user_content(a, b),
        };
        obj.insert("content".into(), merged);
    } else {
        out.push(json!({ "role": "assistant", "content": content }));
    }
}

fn assistant_to_anthropic(obj: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let text = match obj.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(c) => message_content_as_string(c)?,
    };
    if text.is_empty() {
        Ok(Value::String(String::new()))
    } else {
        Ok(Value::String(text))
    }
}

pub fn openai_style_to_anthropic(messages: Vec<Value>) -> Result<(Option<String>, Vec<Value>), String> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();

    for msg in messages {
        let obj = msg
            .as_object()
            .ok_or_else(|| "cada mensaje debe ser un objeto JSON".to_string())?;
        let role = obj
            .get("role")
            .and_then(|r| r.as_str())
            .ok_or_else(|| "mensaje sin role".to_string())?;

        match role {
            "system" => {
                if let Some(c) = obj.get("content") {
                    let s = message_content_as_string(c)?;
                    if !s.is_empty() {
                        system_parts.push(s);
                    }
                }
            }
            "user" => {
                let c = obj
                    .get("content")
                    .ok_or_else(|| "mensaje user sin content".to_string())?;
                push_user(&mut out, openai_content_to_anthropic_user(c)?);
            }
            "assistant" => {
                push_assistant(&mut out, assistant_to_anthropic(obj)?);
            }
            other => return Err(format!("rol no soportado para Anthropic: {other}")),
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    if out.is_empty() {
        return Err("no quedaron mensajes user/assistant para Claude".into());
    }
    if out[0].get("role").and_then(|r| r.as_str()) != Some("user") {
        out.insert(0, json!({ "role": "user", "content": "" }));
    }
    Ok((system, out))
}
