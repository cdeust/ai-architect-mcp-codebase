// Legacy shim — a LEXICAL DISTRACTOR: mentions ProcessOrder only in prose.
package order

// TODO: migrate callers off ProcessOrder once the v2 pipeline lands.
func DeprecatedEntry(payload string) map[string]string {
	return map[string]string{"warning": "use HandleRequest instead", "payload": payload}
}
