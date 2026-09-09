use codex_protocol::items::{
    HookPromptFragment, build_hook_prompt_message, parse_hook_prompt_fragment,
    parse_hook_prompt_message,
};
use codex_protocol::models::ResponseItem;

#[test]
fn patched_xml_preserves_hook_content_and_rejects_malformed_fragments() {
    let fragment = HookPromptFragment::from_single_hook("<tag> & text \"quoted\"", "run<&1");
    let message = build_hook_prompt_message(&[fragment]).expect("serialize hook");
    let ResponseItem::Message { id, content, .. } = message else {
        panic!("expected hook message");
    };
    let parsed = parse_hook_prompt_message(id.as_ref(), &content).expect("parse hook");
    assert_eq!(parsed.fragments.len(), 1);
    assert_eq!(parsed.fragments[0].text, "<tag> & text \"quoted\"");
    assert_eq!(parsed.fragments[0].hook_run_id, "run<&1");
    assert!(parse_hook_prompt_fragment("<hook_prompt>").is_none());
    assert!(
        build_hook_prompt_message(&[HookPromptFragment::from_single_hook("text", " ")]).is_none()
    );
}
