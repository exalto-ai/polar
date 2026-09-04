# Reviewable chat suggestions

A complete built-in chat response has one optional action: **Suggest in document**.

The action waits for current editor changes to save, then creates a pending block insertion at the
current caret or selection block. Current wording does not change until the person accepts the
existing suggestion review slip. Reject changes no wording.

The daemon compares the response’s wording revision with the current document before storing the
proposal. A response generated for older wording must be regenerated. Provider and model labels
remain reported claims.

This reuses the editor API, daemon bearer, suggestion store, and review UI. It adds no provider
credential, native transcript, direct-write route, selection replacement, or second capability
system. Only visible assistant text crosses into the suggestion request. Requested thinking and
local chat history do not enter the daemon or proof.
