# Selected-text chat focus

**Focus on selection** captures the current editor selection as plain text and shows it beside the
composer. The person can remove it before Send. A successful response clears it.

The native request labels and bounds this text separately from the full current document. It is a
snapshot, not a live range: later edits do not update it, and it carries no verified provenance.

The same composer may attach PDFs and UTF-8 text files for one request. The app sends their bytes
inline after validating them in both the WebView and native boundary. It never sends a filesystem
path, creates a temporary file, obtains a provider file ID, or stores the original attachment
payload in local chat history, the daemon, a proof, or provenance. A visible filename and size
summary may remain in the local conversation so the person can see what was sent. A provider may
quote or transform an attachment in its visible response, which persists as ordinary AI chat.

Files are not resent with later messages. Attach them again when a follow-up needs the exact source.
Provider processing and retention begin once Send succeeds, and larger context can increase token
use, latency, and cost.
