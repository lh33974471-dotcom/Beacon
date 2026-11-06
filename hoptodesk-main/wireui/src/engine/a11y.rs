use super::dom::{Document, NodeKey};
use accesskit::{Action, Node, NodeId, Rect, Role, Toggled, Tree, TreeUpdate};
#[cfg(target_os = "macos")]
use accesskit_macos::SubclassingAdapter;
#[cfg(target_os = "windows")]
use accesskit_windows::SubclassingAdapter;
use slotmap::Key;
use std::collections::HashMap;

const ROOT_ID: NodeId = NodeId(0);

fn node_id(key: NodeKey) -> NodeId {
    NodeId(key.data().as_ffi().wrapping_add(1))
}

pub fn role_name(doc: &Document, key: NodeKey) -> &'static str {
    match role_for(doc, key) {
        None => "None",
        Some(Role::Button) => "Button",
        Some(Role::CheckBox) => "CheckBox",
        Some(Role::RadioButton) => "RadioButton",
        Some(Role::TextInput) => "TextInput",
        Some(Role::PasswordInput) => "PasswordInput",
        Some(Role::ComboBox) => "ComboBox",
        Some(Role::Link) => "Link",
        Some(Role::Image) => "Image",
        Some(Role::Label) => "Label",
        Some(_) => "GenericContainer",
    }
}

fn role_for(doc: &Document, key: NodeKey) -> Option<Role> {
    let n = doc.arena.get(key)?;
    if n.is_text() {
        return None;
    }
    let ty = n.attr("type").unwrap_or("");
    Some(match n.tag.as_str() {
        "button" => {
            if ty == "checkbox" {
                Role::CheckBox
            } else {
                Role::Button
            }
        }
        "input" => match ty {
            "checkbox" => Role::CheckBox,
            "radio" => Role::RadioButton,
            "password" => Role::PasswordInput,
            _ => Role::TextInput,
        },
        "select" => Role::ComboBox,
        "a" => Role::Link,
        "img" => Role::Image,
        "header" | "caption" => Role::Label,
        _ => {
            if n
                .attr("tabindex")
                .and_then(|v| v.trim().parse::<i32>().ok())
                .map_or(false, |t| t >= 0)
            {
                Role::Button
            } else {
                Role::GenericContainer
            }
        }
    })
}

fn name_for(doc: &Document, key: NodeKey, allow_text: bool) -> String {
    let Some(n) = doc.arena.get(key) else {
        return String::new();
    };
    if let Some(v) = n.attr("aria-label") {
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    if allow_text {
        let text = doc.collect_text(key).split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            return text;
        }
    }
    for attr in ["title", "placeholder", "novalue"] {
        if let Some(v) = n.attr(attr) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    if matches!(n.tag.as_str(), "input" | "textarea" | "select") {
        let implicit = implicit_input_name(doc, key);
        if !implicit.is_empty() {
            return implicit;
        }
    }
    String::new()
}

fn preceding_label_text(doc: &Document, key: NodeKey) -> String {
    let Some(n) = doc.arena.get(key) else {
        return String::new();
    };
    let Some(parent) = n.parent else {
        return String::new();
    };
    let Some(siblings) = doc.arena.get(parent).map(|p| p.children.clone()) else {
        return String::new();
    };
    let Some(at) = siblings.iter().position(|&c| c == key) else {
        return String::new();
    };
    for &prev in siblings[..at].iter().rev() {
        let text = doc.collect_text(prev).split_whitespace().collect::<Vec<_>>().join(" ");
        let text = text.trim().trim_end_matches(':').trim().to_string();
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

fn implicit_input_name(doc: &Document, key: NodeKey) -> String {
    let own = preceding_label_text(doc, key);
    if !own.is_empty() {
        return own;
    }
    match doc.arena.get(key).and_then(|n| n.parent) {
        Some(parent) => preceding_label_text(doc, parent),
        None => String::new(),
    }
}

fn checkbox_under_label(doc: &Document, key: NodeKey) -> Option<NodeKey> {
    if doc.arena.get(key)?.tag != "label" {
        return None;
    }
    doc.descendants(key).into_iter().find(|&d| {
        doc.arena.get(d).map_or(false, |c| {
            (c.tag == "input" || c.tag == "button") && c.attr("type") == Some("checkbox")
        })
    })
}

fn value_for(doc: &Document, key: NodeKey) -> Option<String> {
    let n = doc.arena.get(key)?;
    if n.tag != "input" && n.tag != "select" {
        return None;
    }
    if n.attr("type") == Some("password") {
        return None;
    }
    n.attr("value").map(|v| v.to_string()).filter(|v| !v.is_empty())
}

fn is_interactive(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::CheckBox
            | Role::RadioButton
            | Role::TextInput
            | Role::PasswordInput
            | Role::ComboBox
            | Role::Link
    )
}

pub fn node_summaries(
    doc: &Document,
    rects: &HashMap<NodeKey, (f32, f32, f32, f32)>,
    scale: f32,
) -> Vec<(String, Option<String>, Option<String>, Option<String>)> {
    let mut out = Vec::new();
    visit(doc, rects, scale, doc.root, &mut out);
    out.iter()
        .map(|(_, n)| {
            (
                format!("{:?}", n.role()),
                n.label().map(|l| l.to_string()),
                n.value().map(|v| v.to_string()),
                n.description().map(|d| d.to_string()),
            )
        })
        .collect()
}

fn visit(
    doc: &Document,
    rects: &HashMap<NodeKey, (f32, f32, f32, f32)>,
    scale: f32,
    key: NodeKey,
    out: &mut Vec<(NodeId, Node)>,
) -> Option<NodeId> {
    let wrapped_checkbox = checkbox_under_label(doc, key);
    let role = match wrapped_checkbox {
        Some(_) => Role::CheckBox,
        None => role_for(doc, key)?,
    };
    let mut children = Vec::new();
    if wrapped_checkbox.is_none() {
        if let Some(n) = doc.arena.get(key) {
            for c in n.children.clone() {
                if let Some(id) = visit(doc, rects, scale, c, out) {
                    children.push(id);
                }
            }
        }
    }
    let name = name_for(doc, key, children.is_empty());
    let description = doc
        .arena
        .get(key)
        .and_then(|n| n.attr("title"))
        .map(|t| t.trim())
        .filter(|t| !t.is_empty() && *t != name)
        .unwrap_or_default()
        .to_string();
    let interactive = is_interactive(role);
    if !interactive && children.is_empty() && name.is_empty() {
        return None;
    }

    let is_static_text = !interactive && children.is_empty();
    let mut node = Node::new(if is_static_text { Role::Label } else { role });
    if !name.is_empty() {
        if is_static_text {
            node.set_value(name);
        } else {
            node.set_label(name);
        }
    }
    if let Some(v) = value_for(doc, key) {
        node.set_value(v);
    }
    if !description.is_empty() {
        node.set_description(description);
    }
    if let Some((x, y, w, h)) = rects.get(&key).copied() {
        if w > 0.0 && h > 0.0 {
            let s = scale as f64;
            node.set_bounds(Rect::new(
                x as f64 * s,
                y as f64 * s,
                (x + w) as f64 * s,
                (y + h) as f64 * s,
            ));
        }
    }
    if matches!(role, Role::CheckBox | Role::RadioButton) {
        let state_src = wrapped_checkbox.unwrap_or(key);
        let checked = doc
            .arena
            .get(state_src)
            .map_or(false, |c| c.states.checked);
        node.set_toggled(if checked { Toggled::True } else { Toggled::False });
    }
    if interactive {
        node.add_action(Action::Focus);
        node.add_action(Action::Click);
    }
    if !children.is_empty() {
        node.set_children(children);
    }
    let id = node_id(key);
    out.push((id, node));
    Some(id)
}

fn focused(doc: &Document) -> Option<NodeId> {
    doc.descendants(doc.root)
        .into_iter()
        .find(|k| {
            doc.arena
                .get(*k)
                .map_or(false, |n| n.states.focus && !n.is_text())
        })
        .map(node_id)
}

fn document_title(doc: &Document) -> String {
    doc.descendants(doc.root)
        .into_iter()
        .find(|k| doc.arena.get(*k).map_or(false, |n| n.tag == "title"))
        .map(|k| doc.collect_text(k).trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "HopToDesk".to_string())
}

pub fn build_update(
    doc: &Document,
    rects: &HashMap<NodeKey, (f32, f32, f32, f32)>,
    scale: f32,
) -> TreeUpdate {
    let title = document_title(doc);
    let mut nodes = Vec::new();
    let mut children = Vec::new();
    if let Some(id) = visit(doc, rects, scale, doc.root, &mut nodes) {
        children.push(id);
    }
    let mut root = Node::new(Role::Window);
    root.set_label(title);
    root.set_children(children);
    nodes.push((ROOT_ID, root));
    let focus = focused(doc).filter(|f| nodes.iter().any(|(id, _)| id == f));
    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT_ID)),
        focus: focus.unwrap_or(ROOT_ID),
    }
}

type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

struct DeferredActivation {
    wake: Wake,
}

impl accesskit::ActivationHandler for DeferredActivation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        NEEDS_FULL_TREE.with(|f| f.set(true));
        (self.wake)();
        None
    }
}

pub enum PendingAction {
    Click,
    Focus,
}

const MAX_PENDING: usize = 32;

static PENDING: std::sync::Mutex<Vec<(Action, NodeId)>> = std::sync::Mutex::new(Vec::new());

struct ForwardActions {
    wake: Wake,
}

impl accesskit::ActionHandler for ForwardActions {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        if !matches!(request.action, Action::Click | Action::Focus) {
            return;
        }
        if let Ok(mut q) = PENDING.lock() {
            if q.len() >= MAX_PENDING {
                return;
            }
            q.push((request.action, request.target));
        }
        (self.wake)();
    }
}

pub fn take_pending(doc: &Document) -> Vec<(PendingAction, NodeKey)> {
    let queued = match PENDING.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return Vec::new(),
    };
    if queued.is_empty() {
        return Vec::new();
    }
    let live = doc.descendants(doc.root);
    queued
        .into_iter()
        .filter_map(|(action, id)| {
            let key = live.iter().copied().find(|k| node_id(*k) == id)?;
            let action = match action {
                Action::Click => PendingAction::Click,
                Action::Focus => PendingAction::Focus,
                _ => return None,
            };
            Some((action, key))
        })
        .collect()
}

struct A11y {
    adapter: SubclassingAdapter,
    last: Option<(u64, Option<NodeId>)>,
}

thread_local! {
    static NEEDS_FULL_TREE: std::cell::Cell<bool> = std::cell::Cell::new(false);
}

thread_local! {
    static ADAPTER: std::cell::RefCell<Option<A11y>> = std::cell::RefCell::new(None);
}

#[cfg(target_os = "windows")]
pub fn install(hwnd: isize, wake: Wake) {
    if hwnd == 0 {
        return;
    }
    ADAPTER.with(|a| {
        let mut a = a.borrow_mut();
        if a.is_some() {
            return;
        }
        let adapter = SubclassingAdapter::new(
            accesskit_windows::HWND(hwnd as _),
            DeferredActivation { wake: wake.clone() },
            ForwardActions { wake },
        );
        *a = Some(A11y { adapter, last: None });
    });
}

#[cfg(target_os = "macos")]
pub fn install(ns_view: *mut std::ffi::c_void, wake: Wake) {
    if ns_view.is_null() {
        return;
    }
    ADAPTER.with(|a| {
        let mut a = a.borrow_mut();
        if a.is_some() {
            return;
        }
        let adapter =
            unsafe {
                SubclassingAdapter::new(
                    ns_view,
                    DeferredActivation { wake: wake.clone() },
                    ForwardActions { wake },
                )
            };
        *a = Some(A11y { adapter, last: None });
    });
}

pub fn push(doc: &Document, rects: &HashMap<NodeKey, (f32, f32, f32, f32)>, scale: f32) {
    ADAPTER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(a11y) = slot.as_mut() else {
            return;
        };
        let forced = NEEDS_FULL_TREE.with(|f| f.replace(false));
        let key = (super::dom::layout_epoch(), focused(doc));
        if !forced && a11y.last.as_ref() == Some(&key) {
            return;
        }
        a11y.last = Some(key);
        if let Some(events) = a11y
            .adapter
            .update_if_active(|| build_update(doc, rects, scale))
        {
            events.raise();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::{ActionHandler, ActivationHandler};

    fn doc_with_two_buttons() -> crate::engine::host::EngineRef {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "{}",
            r#"<html><body><button #a>Alpha</button><button #b>Beta</button></body></html>"#
        )
        .unwrap();
        let path = f.into_temp_path();
        crate::engine::host::load_page_bridged(std::path::Path::new(&path), None, "OSX", None)
            .unwrap()
            .engine
    }

    fn key_of(e: &crate::engine::host::EngineRef, id_attr: &str) -> NodeKey {
        let eb = e.borrow();
        eb.doc
            .descendants(eb.doc.root)
            .into_iter()
            .find(|k| eb.doc.arena.get(*k).and_then(|n| n.attr("id")) == Some(id_attr))
            .unwrap()
    }

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        PENDING.lock().unwrap_or_else(|e| e.into_inner()).clear();
        g
    }

    fn wake_counter() -> (Wake, std::sync::Arc<std::sync::atomic::AtomicU32>) {
        let n = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = n.clone();
        (
            std::sync::Arc::new(move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
            n,
        )
    }

    #[test]
    fn a_screen_reader_click_resolves_to_the_right_node_and_drains_once() {
        let _serial = serial();
        let e = doc_with_two_buttons();
        let beta = key_of(&e, "b");
        let (wake, woke) = wake_counter();
        let mut handler = ForwardActions { wake };

        handler.do_action(accesskit::ActionRequest {
            action: Action::Click,
            target: node_id(beta),
            data: None,
        });

        let eb = e.borrow();
        let drained = take_pending(&eb.doc);
        assert_eq!(drained.len(), 1);
        assert!(matches!(drained[0].0, PendingAction::Click));
        assert_eq!(drained[0].1, beta, "must resolve to the node the reader named");
        assert_eq!(woke.load(std::sync::atomic::Ordering::SeqCst), 1);

        assert!(
            take_pending(&eb.doc).is_empty(),
            "draining must consume the queue or the click repeats every frame"
        );
    }

    #[test]
    fn unknown_and_unsupported_actions_are_dropped() {
        let _serial = serial();
        let e = doc_with_two_buttons();
        let (wake, _woke) = wake_counter();
        let mut handler = ForwardActions { wake };

        handler.do_action(accesskit::ActionRequest {
            action: Action::Click,
            target: NodeId(u64::MAX),
            data: None,
        });
        handler.do_action(accesskit::ActionRequest {
            action: Action::ScrollUp,
            target: node_id(key_of(&e, "a")),
            data: None,
        });

        let eb = e.borrow();
        assert!(take_pending(&eb.doc).is_empty());
    }

    #[test]
    fn the_queue_is_bounded_when_the_ui_thread_never_drains() {
        let _serial = serial();
        let e = doc_with_two_buttons();
        let alpha = key_of(&e, "a");
        let (wake, _woke) = wake_counter();
        let mut handler = ForwardActions { wake };

        for _ in 0..(MAX_PENDING * 4) {
            handler.do_action(accesskit::ActionRequest {
                action: Action::Click,
                target: node_id(alpha),
                data: None,
            });
        }
        assert_eq!(PENDING.lock().unwrap().len(), MAX_PENDING);
        let eb = e.borrow();
        take_pending(&eb.doc);
    }

    #[test]
    fn activation_requests_a_full_tree_and_wakes_the_event_loop() {
        let (wake, woke) = wake_counter();
        let mut handler = DeferredActivation { wake };

        NEEDS_FULL_TREE.with(|f| f.set(false));
        assert!(handler.request_initial_tree().is_none());

        assert!(
            NEEDS_FULL_TREE.with(|f| f.get()),
            "activation must force the next push to send a full tree"
        );
        assert_eq!(
            woke.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "activation must request a redraw, or an idle window never pushes the tree"
        );
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    fn doc() -> crate::engine::host::EngineRef {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "{}",
            r#"<html><body>
                 <button #go>Accept and install</button>
                 <input type="checkbox" #ico title="Create desktop icon" />
                 <input type="password" #pw value="hunter2" />
               </body></html>"#
        )
        .unwrap();
        let path = f.into_temp_path();
        crate::engine::host::load_page_bridged(std::path::Path::new(&path), None, "OSX", None)
            .unwrap()
            .engine
    }

    fn tree_of(page: &str) -> TreeUpdate {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", page).unwrap();
        let path = f.into_temp_path();
        let e = crate::engine::host::load_page_bridged(std::path::Path::new(&path), None, "OSX", None)
            .unwrap()
            .engine;
        let eb = e.borrow();
        build_update(&eb.doc, &HashMap::new(), 1.0)
    }

    #[test]
    fn a_container_is_not_named_with_all_its_descendant_text() {
        let update = tree_of(
            r#"<html><body><div id="row"><button>Connect</button><button>Transfer</button></div></body></html>"#,
        );
        let joined: Vec<String> = update
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().map(|s| s.to_string()))
            .collect();
        assert!(
            joined.iter().any(|l| l == "Connect") && joined.iter().any(|l| l == "Transfer"),
            "each control must still carry its own label, got {:?}",
            joined
        );
        assert!(
            !joined.iter().any(|l| l.contains("Connect") && l.contains("Transfer")),
            "a container must not be named with its children's text, or the reader recites the \
             whole screen at every nesting level: {:?}",
            joined
        );
    }

    #[test]
    fn a_checkbox_reports_whether_it_is_checked() {
        let update = tree_of(
            r#"<html><body>
                 <button type="checkbox" id="on" checked>Create desktop icon</button>
                 <button type="checkbox" id="off">Create start menu shortcuts</button>
               </body></html>"#,
        );
        let states: Vec<(String, Option<Toggled>)> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::CheckBox)
            .map(|(_, n)| (n.label().unwrap_or("").to_string(), n.toggled()))
            .collect();
        assert_eq!(states.len(), 2, "both checkboxes must be in the tree: {:?}", states);
        for (label, toggled) in &states {
            assert!(
                toggled.is_some(),
                "a screen reader cannot announce checked/unchecked without a state: {:?}",
                label
            );
        }
        assert!(states.iter().any(|(_, t)| *t == Some(Toggled::True)));
        assert!(states.iter().any(|(_, t)| *t == Some(Toggled::False)));
    }

    #[test]
    fn a_label_wrapping_a_hidden_checkbox_becomes_one_named_checkbox() {
        let update = tree_of(
            r#"<html><body><div>
                 <span>Unattended Access</span>
                 <label title="Toggle unattended access"><input type="checkbox" id="cb" checked /><span/></label>
               </div></body></html>"#,
        );
        let boxes: Vec<(String, Option<Toggled>)> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::CheckBox)
            .map(|(_, n)| (n.label().unwrap_or("").to_string(), n.toggled()))
            .collect();
        assert_eq!(boxes.len(), 1, "the label and its hidden input must collapse to one control: {:?}", boxes);
        assert_eq!(boxes[0].0, "Toggle unattended access");
        assert_eq!(boxes[0].1, Some(Toggled::True));
    }

    #[test]
    fn an_input_takes_its_name_from_the_nearest_preceding_label() {
        let update = tree_of(
            r#"<html><body>
                 <div>Installation Path :</div>
                 <div><input type="text" id="path" /></div>
                 <div><span>Hostname:</span><input type="text" id="host" /></div>
               </body></html>"#,
        );
        let names: Vec<String> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::TextInput)
            .map(|(_, n)| n.label().unwrap_or("").to_string())
            .collect();
        assert!(names.contains(&"Installation Path".to_string()), "got {:?}", names);
        assert!(names.contains(&"Hostname".to_string()), "got {:?}", names);
    }

    #[test]
    fn the_tree_carries_named_controls_and_never_the_password() {
        let e = doc();
        let eb = e.borrow();
        let update = build_update(&eb.doc, &HashMap::new(), 1.0);

        let labels: Vec<String> = update
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().map(|s| s.to_string()))
            .collect();
        assert!(
            labels.iter().any(|l| l == "Accept and install"),
            "a screen reader must hear the button label, got {:?}",
            labels
        );
        assert!(
            labels.iter().any(|l| l == "Create desktop icon"),
            "a checkbox must be named from its title, got {:?}",
            labels
        );
        assert!(
            update.nodes.iter().any(|(_, n)| n.role() == Role::Button),
            "the button must keep its role"
        );
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, n)| n.value().map_or(false, |v| v.contains("hunter2"))),
            "a password value must never be exposed to the a11y tree"
        );
    }
}
