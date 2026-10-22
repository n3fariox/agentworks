# Host API v1. Formats data only; Rust owns attribution and the trust notice.
MessageContext = record(channel = str, state = dict[str, typing.Any])
PeerMessage = record(id = int, sender = str, sender_title = str, text = str, truncated = bool)

def message_from_json(data: dict[str, typing.Any]) -> PeerMessage:
    return PeerMessage(
        id = data["id"],
        sender = data["sender"],
        sender_title = data["senderTitle"],
        text = data["text"],
        truncated = data.get("truncated", False),
    )
