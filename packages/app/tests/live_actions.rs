use hyperchad::{
    actions::{ActionTrigger, ActionType},
    template::container,
};

#[test]
fn shared_state_refresh_and_lifecycle_events_use_distinct_elements() {
    let page = container! {
        div fx-global-shared-state-event=(ActionType::Navigate {
            url: "/games/one".to_string(),
        }) {
            span fx-global-shared-state-connected=(ActionType::NoOp) { "Connected" }
        }
    };
    let root = page.first().expect("page has a root");
    let child = root.children.first().expect("page has a status child");

    assert!(root.actions.iter().any(|action| matches!(
        &action.trigger,
        ActionTrigger::Event(name) if name == "shared-state-event"
    )));
    assert!(root.actions.iter().any(|action| matches!(
        &action.effect.action,
        ActionType::Navigate { url } if url == "/games/one"
    )));
    assert!(child.actions.iter().any(|action| matches!(
        &action.trigger,
        ActionTrigger::Event(name) if name == "shared-state-connected"
    )));
}
