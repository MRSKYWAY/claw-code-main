use runtime::{ContentBlock, ConversationMessage, Session};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(suffix: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("claw-runtime-session-{nanos}-{suffix}.json"))
}

#[test]
fn saves_session_atomically_and_removes_temp_file() {
    let path = temp_path("atomic");
    let temp_path = path.with_extension("json.tmp");

    let mut session = Session::new();
    session
        .messages
        .push(ConversationMessage::user_text("recover me"));
    session.messages.push(ConversationMessage::assistant(vec![
        ContentBlock::Text {
            text: "state is intact".to_string(),
        },
    ]));

    session.save_to_path(&path).expect("session should save");

    let restored = Session::load_from_path(&path).expect("session should load");
    assert_eq!(restored, session);
    assert!(
        !temp_path.exists(),
        "atomic save must not leave its temporary file behind"
    );

    fs::remove_file(path).expect("session file should be removable");
}
