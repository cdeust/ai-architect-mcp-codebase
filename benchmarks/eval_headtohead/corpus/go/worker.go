// Background worker — a real caller of ProcessOrder (D2 ground truth).
package order

func Drain(queue []map[string]string) []map[string]string {
	out := make([]map[string]string, 0, len(queue))
	for _, order := range queue {
		out = append(out, ProcessOrder(order))
	}
	return out
}
