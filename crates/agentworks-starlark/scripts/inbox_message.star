load("@agentworks//message.star", "MessageContext", "PeerMessage")

# Rust adds the message ID, sender identity and trust boundary around this
# Markdown. Preserve the peer's paragraphs, lists, links and code blocks.
def format_message(ctx: MessageContext, message: PeerMessage) -> str:
    text = message.text
    if message.truncated:
        text += "\n\n*Message truncated. Read the inbox for the full text.*"
    return text

agent.register_formatter(format_message, message_type = "inbox.peer")
