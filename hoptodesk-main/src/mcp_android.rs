use serde_json::{json, Value};

pub fn handle_mcp_request(json_str: &str) -> Option<String> {
    let request: Value = serde_json::from_str(json_str).ok()?;
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let response = match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "hoptodesk-android", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
        "tools/list" => json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools_list() } }),
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call_tool(name, &args) {
                Ok(content) => json!({ "jsonrpc": "2.0", "id": id, "result": { "content": content } }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": [{ "type": "text", "text": e }], "isError": true }
                }),
            }
        }
        _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } }),
    };
    serde_json::to_string(&response).ok()
}

#[allow(unused_variables)]
fn call_tool(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "get_device_info" => {
            let info = json!({
                "device_id": hbb_common::config::Config::get_id(),
                "version": env!("CARGO_PKG_VERSION"),
                "platform": "android",
            });
            Ok(json!([{ "type": "text", "text": serde_json::to_string_pretty(&info).unwrap_or_default() }]))
        }
        "get_device_health" => match scrap::android::call_main_service_get_by_name("device_health") {
            Ok(s) => Ok(json!([{ "type": "text", "text": s }])),
            Err(e) => Err(format!("device_health failed: {:?}", e)),
        },
        "android_list_apps" => match scrap::android::call_main_service_get_by_name("list_apps") {
            Ok(s) => Ok(json!([{ "type": "text", "text": s }])),
            Err(e) => Err(format!("list_apps failed: {:?}", e)),
        },
        "android_launch_app" => {
            let pkg = args.get("package").and_then(|p| p.as_str()).unwrap_or("");
            if pkg.is_empty() {
                return Err("package is required".to_string());
            }
            match scrap::android::call_main_service_set_by_name("launch_app", Some(pkg), None) {
                Ok(_) => Ok(json!([{ "type": "text", "text": format!("launched {}", pkg) }])),
                Err(e) => Err(format!("launch_app failed: {:?}", e)),
            }
        }
        "android_wake_screen" => {
            match scrap::android::call_main_service_set_by_name("wake_screen", Some(""), None) {
                Ok(_) => Ok(json!([{ "type": "text", "text": "screen woken" }])),
                Err(e) => Err(format!("wake_screen failed: {:?}", e)),
            }
        }
        "android_reboot" => {
            match scrap::android::call_main_service_set_by_name("reboot_device", Some(""), None) {
                Ok(_) => Ok(json!([{ "type": "text", "text": "reboot requested (requires device owner)" }])),
                Err(e) => Err(format!("reboot failed: {:?}", e)),
            }
        }
        #[cfg(not(feature = "playstore"))]
        "android_global_action" => {
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("home");
            agent_call("global_action", &json!({ "action": action }))?;
            Ok(json!([{ "type": "text", "text": format!("global_action {} dispatched", action) }]))
        }
        #[cfg(not(feature = "playstore"))]
        "android_tap" | "mouse_click" | "click_text" => agent_text("tap", args),
        #[cfg(not(feature = "playstore"))]
        "android_swipe" => agent_text("swipe", args),
        #[cfg(not(feature = "playstore"))]
        "mouse_drag" => agent_text(
            "swipe",
            &json!({ "x1": args["start_x"], "y1": args["start_y"], "x2": args["end_x"], "y2": args["end_y"] }),
        ),
        #[cfg(not(feature = "playstore"))]
        "scroll" => agent_text("scroll", args),
        #[cfg(not(feature = "playstore"))]
        "get_screen_size" => agent_text("screen_size", args),
        #[cfg(not(feature = "playstore"))]
        "key_press" => {
            let key = args.get("key").and_then(|k| k.as_str()).unwrap_or("").to_lowercase();
            let action = match key.as_str() {
                "back" | "escape" | "esc" => "back",
                "home" => "home",
                "recents" | "app_switch" => "recents",
                _ => return Err("On Android key_press supports back, home and recents. Use type_text for text.".to_string()),
            };
            agent_call("global_action", &json!({ "action": action }))?;
            Ok(json!([{ "type": "text", "text": format!("{} pressed", action) }]))
        }
        #[cfg(not(feature = "playstore"))]
        "type_text" => agent_text("type_text", args),
        #[cfg(not(feature = "playstore"))]
        "read_screen" | "read_screen_text" => agent_text("read_screen", args),
        #[cfg(not(feature = "playstore"))]
        "get_foreground_app" => agent_text("foreground_app", args),
        #[cfg(not(feature = "playstore"))]
        "screenshot" => {
            let mut shot = agent_call("screenshot", args)?;
            let data = shot.get("data").and_then(|d| d.as_str()).unwrap_or("").to_string();
            if let Some(o) = shot.as_object_mut() {
                o.remove("data");
            }
            Ok(json!([
                { "type": "image", "data": data, "mimeType": "image/jpeg" },
                { "type": "text", "text": shot.to_string() }
            ]))
        }
        _ => Err(format!("Unknown tool: {}", name)),
    }
}

#[cfg(not(feature = "playstore"))]
fn agent_call(name: &str, args: &Value) -> Result<Value, String> {
    let raw = scrap::android::call_main_service_agent(name, &args.to_string())
        .map_err(|e| format!("{} failed: {:?}", name, e))?;
    let reply: Value = serde_json::from_str(&raw).map_err(|_| format!("{} returned no result", name))?;
    match reply.get("error").and_then(|e| e.as_str()) {
        Some(e) => Err(e.to_string()),
        None => Ok(reply),
    }
}

#[cfg(not(feature = "playstore"))]
fn agent_text(name: &str, args: &Value) -> Result<Value, String> {
    let reply = agent_call(name, args)?;
    Ok(json!([{ "type": "text", "text": reply.to_string() }]))
}

fn tools_list() -> Value {
    let mut tools = vec![
        json!({ "name": "get_device_info", "description": "Get device id, app version, and platform", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "get_device_health", "description": "Battery, storage, model, Android version, and whether the accessibility service that the screen and input tools need is on", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "android_list_apps", "description": "List installed launchable apps with package, label, and version", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "android_launch_app", "description": "Launch or bring an app to the foreground by package name", "inputSchema": { "type": "object", "properties": { "package": { "type": "string" } } } }),
        json!({ "name": "android_wake_screen", "description": "Wake the device screen", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "android_reboot", "description": "Reboot the device. Requires the app to be provisioned as device owner via ADB; no-ops otherwise", "inputSchema": { "type": "object", "properties": {} } }),
    ];
    #[cfg(not(feature = "playstore"))]
    {
        tools.push(json!({ "name": "android_global_action", "description": "Navigation action: home/back/recents/notifications/quick_settings/lock_screen", "inputSchema": { "type": "object", "properties": { "action": { "type": "string" } } } }));
        tools.push(json!({ "name": "read_screen", "description": "List the visible elements on screen as text: label, class, bounds [left, top, right, bottom] in screen pixels, and whether each is clickable or editable. Cheaper and more exact than screenshot; use it first", "inputSchema": { "type": "object", "properties": {} } }));
        tools.push(json!({ "name": "screenshot", "description": "Capture the screen as a JPEG (Android 11+, at most one per second). The reply gives image_width/image_height and screen_width/screen_height; when they differ, scale image coordinates up to screen pixels before tapping. Protected windows such as banking apps come back black", "inputSchema": { "type": "object", "properties": {} } }));
        tools.push(json!({ "name": "android_tap", "description": "Tap the first visible element whose label matches text, or tap x,y in screen pixels", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" }, "x": { "type": "integer" }, "y": { "type": "integer" } } } }));
        tools.push(json!({ "name": "android_swipe", "description": "Swipe from x1,y1 to x2,y2 in screen pixels. Swipe up to scroll down", "inputSchema": { "type": "object", "properties": { "x1": { "type": "integer" }, "y1": { "type": "integer" }, "x2": { "type": "integer" }, "y2": { "type": "integer" }, "duration_ms": { "type": "integer" } }, "required": ["x1", "y1", "x2", "y2"] } }));
        tools.push(json!({ "name": "type_text", "description": "Replace the content of the focused text field. Tap the field first", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } }));
        tools.push(json!({ "name": "get_foreground_app", "description": "Package name of the app in the foreground", "inputSchema": { "type": "object", "properties": {} } }));
    }
    Value::Array(tools)
}
