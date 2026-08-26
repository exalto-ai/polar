/**
 * The development connection endpoint returns live bearer capabilities. It is
 * safe only to the same machine, even when Vite is intentionally bound to a
 * LAN interface for other assets.
 */
export function isLoopbackAddress(address: string | undefined): boolean {
  if (!address) return false;
  const normalized = address.toLowerCase().split("%")[0];
  if (normalized === "::1" || normalized === "0:0:0:0:0:0:0:1") return true;
  const ipv4 = normalized.startsWith("::ffff:")
    ? normalized.slice("::ffff:".length)
    : normalized;
  return /^127(?:\.\d{1,3}){3}$/.test(ipv4);
}
