use crate::{
    Ledger, Store,
    mail::{DeliveryDecision, NewMessage, SendOutcome},
};

#[test]
fn message_reply_and_wake_commit_together_or_not_at_all() {
    let mut db = Store::memory().unwrap();
    db.connection
        .execute_batch("INSERT INTO sessions VALUES ('sender','{}','{}'),('recipient','{}','{}');")
        .unwrap();
    let mut message = NewMessage {
        sender: "sender",
        sender_title: "Sender",
        recipient: "recipient",
        recipient_title: "Recipient",
        text: "hello",
        reply_to: None,
        request_id: "one",
        expect_reply: true,
        enqueue: true,
        created: 1,
    };
    // A policy read/decision failure must not leave a message without its wake
    // or an outstanding reply request that the sender never received.
    assert!(
        db.send_message(
            &message,
            |_, _, _| Err("fixture policy read failure".into())
        )
        .is_err()
    );
    for table in [
        "agent_messages",
        "reply_requests",
        "inbox_deliveries",
        "agent_audit",
    ] {
        let count: i64 = db
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "partial write in {table}");
    }
    assert_eq!(
        db.send_message(&message, |_, reply, pending| {
            assert!(!reply);
            assert_eq!(pending, 0);
            Ok(DeliveryDecision::Queue { generation: 7 })
        })
        .unwrap(),
        SendOutcome::Inserted(1)
    );
    assert_eq!(
        db.send_message(&message, |_, _, _| panic!("a retry must not enroll twice"))
            .unwrap(),
        SendOutcome::Duplicate(1)
    );
    message.text = "different payload";
    assert_eq!(
        db.send_message(&message, |_, _, _| panic!("conflicts must not enroll"))
            .unwrap(),
        SendOutcome::Conflict
    );
    assert_eq!(db.inbox().pending("recipient").unwrap(), 1);
    assert_eq!(db.acknowledge("recipient", Some(1)).unwrap(), 1);
    assert_eq!(db.inbox().pending("recipient").unwrap(), 0);
    assert_eq!(db.unacknowledged("recipient").unwrap(), 0);
    assert_eq!(
        db.inbox().deliveries("recipient").unwrap()[0]["state"],
        "cancelled"
    );
}

#[test]
fn repository_views_share_one_rollback_boundary() {
    let mut db = Store::memory().unwrap();
    db.connection
        .execute("INSERT INTO sessions VALUES ('s','{}','{}')", [])
        .unwrap();
    {
        let tx = db.transaction().unwrap();
        tx.inbox()
            .save_policy("s", &serde_json::json!({"enabled":true}))
            .unwrap();
        tx.reviews()
            .append_event("event", "automation", 1, "{}", 1)
            .unwrap();
        // Dropping any repository's transaction rolls back every repository.
    }
    assert_eq!(db.inbox().policy::<serde_json::Value>("s").unwrap(), None);
    assert!(
        db.pending_review_events("automation", 1)
            .unwrap()
            .is_empty()
    );
    {
        let tx = db.transaction().unwrap();
        tx.inbox()
            .save_policy("s", &serde_json::json!({"enabled":true}))
            .unwrap();
        tx.reviews()
            .append_event("event", "automation", 1, "{}", 1)
            .unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        db.inbox()
            .policy::<serde_json::Value>("s")
            .unwrap()
            .unwrap()["enabled"],
        true
    );
    assert_eq!(db.pending_review_events("automation", 1).unwrap().len(), 1);
}

#[test]
fn rejected_automation_save_rolls_back_its_script_and_request_ledger() {
    let mut db = Store::memory().unwrap();
    db.connection.execute_batch("CREATE TRIGGER reject_definition BEFORE INSERT ON automations BEGIN SELECT RAISE(ABORT,'fixture storage failure'); END;").unwrap();
    let definition = serde_json::json!({"id":"a","enabled":true,"updated":1,"revision":1,
        "workflow":{"trigger":{"kind":"script","script":"a.star"}}});
    let ledger = Ledger {
        actor: "actor",
        request_id: "request",
        operation: "save",
        payload: "{}",
        result: "{}",
    };
    assert!(
        db.save_automation_source(&definition.to_string(), None, Some(&ledger), Some("source"))
            .is_err()
    );
    assert!(db.script_source("a.star").unwrap().is_none());
    assert!(db.automation_request("actor", "request").unwrap().is_none());
    assert!(db.automation::<serde_json::Value>("a").unwrap().is_none());
}
