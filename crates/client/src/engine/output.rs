// Keep the existing inline budget until the backend's message/history limits are raised.
const INLINE_LIMIT: usize = protocol::TOOL_OUTPUT_BYTES;

/// Preserve both ends of captured output, with an explicit omission notice and valid UTF-8.
pub(crate) fn preview(text: &str) -> String {
    if text.len()<=INLINE_LIMIT { return text.to_owned(); }
    let notice="\n[Output truncated: middle omitted. Captured stdout/stderr are available in Tasks and the saved process record (up to 1 MiB each). Do not repeat side effects to recover output.]\n";
    let budget=INLINE_LIMIT-notice.len();
    let mut start=budget/2;
    let mut end=text.len()-(budget-start);
    while !text.is_char_boundary(start) { start-=1; }
    while !text.is_char_boundary(end) { end+=1; }
    format!("{}{notice}{}",&text[..start],&text[end..])
}
