/** Copy text in both current browsers and older installed WKWebView builds. */
export async function writeClipboardText(text: string): Promise<void> {
  let modernRejected = false;
  let modernError: unknown;
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch (error) {
      modernRejected = true;
      modernError = error;
    }
  }

  const priorFocus = document.activeElement instanceof HTMLElement
    ? document.activeElement
    : null;
  const proxy = document.createElement("textarea");
  proxy.value = text;
  proxy.setAttribute("readonly", "");
  proxy.style.position = "fixed";
  proxy.style.opacity = "0";
  document.body.append(proxy);
  let fallbackError: unknown;
  try {
    proxy.select();
    const copied = document.execCommand?.("copy") ?? false;
    if (!copied) fallbackError = new Error("copy is unavailable");
  } catch (error) {
    fallbackError = error;
  } finally {
    proxy.remove();
    if (priorFocus?.isConnected) priorFocus.focus({ preventScroll: true });
  }
  if (fallbackError !== undefined) {
    if (modernRejected) throw modernError;
    throw fallbackError;
  }
}
