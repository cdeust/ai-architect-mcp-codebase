// Reporting — a LEXICAL DISTRACTOR module (§3 pre-registration).
// Mentions ProcessOrder in a comment and a string, and defines
// substring-colliding names (PreprocessOrder, ProcessOrders). None of these
// are real call sites of ProcessOrder.
package order

// NOTE: ProcessOrder is the hot path; report on its throughput here.
const Label = "ProcessOrder latency"

func PreprocessOrder(order map[string]string) map[string]string {
	out := map[string]string{}
	for k, v := range order {
		out[k] = v
	}
	return out
}

func ProcessOrders(orders []map[string]string) []map[string]string {
	out := make([]map[string]string, 0, len(orders))
	for _, o := range orders {
		out = append(out, PreprocessOrder(o))
	}
	return out
}
