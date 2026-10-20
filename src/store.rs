//! Tiny JSON persistence under %APPDATA%\winnotch (falls back to USERPROFILE / HOME).

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::features::{Task, Water};

fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("winnotch"))
}

fn read(name: &str) -> Option<Value> {
    let text = std::fs::read_to_string(dir()?.join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write(name: &str, v: &Value) {
    if let Some(d) = dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(name), v.to_string());
    }
}

pub fn load_tasks() -> Vec<Task> {
    let Some(v) = read("todo.json") else {
        return Vec::new();
    };
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    Some(Task {
                        text: t.get("text")?.as_str()?.to_string(),
                        due: t.get("due").and_then(|d| d.as_str()).map(|s| s.to_string()),
                        done: t.get("done").and_then(|d| d.as_bool()).unwrap_or(false),
                        notified: t.get("notified").and_then(|d| d.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn save_tasks(tasks: &[Task]) {
    let v: Vec<Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "text": t.text,
                "due": t.due,
                "done": t.done,
                "notified": t.notified,
            })
        })
        .collect();
    write("todo.json", &Value::Array(v));
}

pub fn load_water(today: &str) -> Water {
    let mut w = Water::new(today);
    if let Some(v) = read("water.json") {
        let num = |k: &str| v.get(k).and_then(|x| x.as_u64()).map(|x| x as u32);
        if let Some(x) = num("goal_ml") {
            w.goal_ml = x;
        }
        if let Some(x) = num("bottle_ml") {
            w.bottle_ml = x;
        }
        if let Some(x) = num("interval_min") {
            w.interval_min = x;
        }
        if let Some(x) = num("from_hour") {
            w.from_hour = x;
        }
        if let Some(x) = num("to_hour") {
            w.to_hour = x;
        }
        if v.get("date").and_then(|d| d.as_str()) == Some(today) {
            w.drank_ml = num("drank_ml").unwrap_or(0);
        }
    }
    w
}

pub fn save_water(w: &Water) {
    write(
        "water.json",
        &json!({
            "goal_ml": w.goal_ml,
            "bottle_ml": w.bottle_ml,
            "interval_min": w.interval_min,
            "from_hour": w.from_hour,
            "to_hour": w.to_hour,
            "date": w.date,
            "drank_ml": w.drank_ml,
        }),
    );
}
