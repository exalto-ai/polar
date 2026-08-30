# Selected-text chat focus

**Focus on selection** captures the editor's current selection as plain text for the next built-in
chat request. The captured text stays visible beside the composer and can be removed before Send.

The full current document is still sent. Focus tells the provider what part to prioritize; it is
not a privacy filter and does not grant document-write authority. Native code bounds the snapshot,
marks it as untrusted source material in the request, and stores it with the visible turn. Retry
uses the same focus with a fresh current-document snapshot.

This feature adds no file picker, attachment lifecycle, durable editor anchor, direct write,
provider credential, or verified provenance claim.
