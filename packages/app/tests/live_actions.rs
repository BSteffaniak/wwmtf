use hyperchad::{
    actions::{ActionTrigger, ActionType},
    template::container,
};

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
