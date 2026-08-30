# Built-in provider transport

The native window owns provider credentials, HTTPS requests, document projection, and response
parsing. JavaScript sends a fixed provider identifier, model identifier, visible conversation,
the current editor tree, and a versioned sharing acknowledgement. It never receives a key.

Opening chat may read the selected provider’s model catalog. This state is not persisted and is
not presented as proof that a later generation request will work. Sending validates and projects
the current editor tree to bounded Markdown, then calls a fixed OpenAI or Anthropic endpoint with
redirects disabled.

The transport is intentionally non-streaming and non-persistent. It has no transcript database,
file locks, restart recovery, thinking controls, Stop, Retry, or hidden reasoning surface. It
returns bounded visible text, the requested and reported model labels, the wording revision sent,
and whether the provider reported a complete response.

An optional selected-text focus is sent as a separate bounded plain-text field. It never becomes a
file attachment or durable editor range.
