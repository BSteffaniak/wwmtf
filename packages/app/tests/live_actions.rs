use hyperchad::{
    actions::{ActionTrigger, ActionType},
    template::container,
};

#[test]
fn multiple_shared_state_events_use_distinct_elements() {
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
    assert!(child.actions.iter().any(|action| matches!(
        &action.trigger,
        ActionTrigger::Event(name) if name == "shared-state-connected"
    )));
}

#[test]
fn shared_state_event_action_is_renderer_neutral() {
    let page = container! {
        div fx-global-shared-state-event=(ActionType::Navigate {
            url: "/games/one".to_string(),
        }) { "Game" }
    };
    let page = page.first().expect("page has a root");

    assert!(page.actions.iter().any(|action| matches!(
        &action.trigger,
        ActionTrigger::Event(name) if name == "shared-state-event"
    )));
    assert!(page.actions.iter().any(|action| matches!(
        &action.effect.action,
        ActionType::Navigate { url } if url == "/games/one"
    )));
}

#[test]
fn shared_state_update_action_is_renderer_neutral() {
    let page = container! {
        div fx-global-shared-state-update=(ActionType::Navigate {
            url: "/games/one".to_string(),
        }) { "Game" }
    };
    let page = page.first().expect("page has a root");

    assert!(page.actions.iter().any(|action| matches!(
        &action.trigger,
        ActionTrigger::Event(name) if name == "shared-state-update"
    )));
    assert!(page.actions.iter().any(|action| matches!(
        &action.effect.action,
        ActionType::Navigate { url } if url == "/games/one"
    )));
}
