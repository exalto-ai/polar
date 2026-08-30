# Reviewable chat suggestions

A completed built-in chat response has one optional action: **Suggest in document**.

The action waits for current editor changes to save, then creates a pending block insertion at the
current caret or selection block. Current wording does not change until the person accepts the
existing suggestion review slip. Reject changes no wording.

Native code reloads the response text and provider metadata from the private transcript. The
webview supplies only the completed turn, an idempotency key, and the insertion position. The
daemon compares the transcript's wording revision with the current document before storing the
proposal. A response generated for older wording must be regenerated.

This uses the existing daemon bearer and suggestion lifecycle. It adds no provider credential,
daemon capability, discovery field, direct-write route, selection replacement, file context, or
verified provider claim. Accepted wording is attributed to the reported built-in provider source;
the local person remains the decision maker.
