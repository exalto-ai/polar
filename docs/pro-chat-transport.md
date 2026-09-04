# Built-in provider transport

The native window owns provider credentials, HTTPS requests, document projection, and response
parsing. JavaScript sends a fixed provider identifier, model identifier, requested thinking level,
visible conversation, the current editor tree, optional focus, and a versioned sharing
acknowledgement. It never receives a key.

Opening chat may read the selected provider's model catalog. This state is not presented as proof
that a later generation request will work. Sending validates and projects the current editor tree
to bounded Markdown, then calls a fixed OpenAI Responses or Anthropic Messages endpoint with
redirects disabled. OpenAI requests keep `store: false`.

Provider default omits an effort setting. Low, Medium, and High map to OpenAI
`reasoning.effort` and Anthropic `output_config.effort`. These are requested settings, not observed
or verified reasoning. Hidden reasoning blocks are never returned to the WebView.

The transport remains non-streaming and has no native transcript database, provider-side
conversation ID, Stop command, or hidden reasoning surface. It returns bounded visible text,
requested and reported model labels, the wording revision sent, and whether the provider reported
a complete response.

Visible history persists per document in bounded WebView local storage. Storage is local-only,
fail-soft convenience state. It contains no draft, focus, or hidden reasoning. An optional
selected-text focus is sent as a separate bounded plain-text field. It never becomes a file
attachment or durable editor range.
