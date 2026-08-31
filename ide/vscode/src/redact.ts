/**
 * Redaction helpers.
 *
 * Deliberately free of any `vscode` import. These are the functions standing
 * between a credential and the output channel, so they must be testable with
 * plain `node --test` rather than only inside an extension host — a security
 * control that can only be exercised by launching an editor is one that stops
 * being exercised.
 */

/**
 * Strip the query and fragment off a URL before it is logged.
 *
 * Gateways sign URLs with a token in the query string, so logging a URL
 * verbatim is the one-line version of leaking a credential. Callers get the
 * origin and path only. An unparseable input yields a placeholder rather than
 * being echoed back — the failure branch must not be the disclosing branch.
 */
export function redact(url: string): string {
  try {
    const parsed = new URL(url);
    return `${parsed.origin}${parsed.pathname}`;
  } catch {
    return '<unparseable url>';
  }
}

/**
 * Reduce an unknown thrown value to a message safe to log.
 *
 * Errors from `fetch` and from `child_process` can carry the offending input —
 * headers, argv — on properties nobody reads deliberately. Only the message
 * survives.
 */
export function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
