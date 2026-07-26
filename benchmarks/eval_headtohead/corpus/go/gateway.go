// Inbound gateway — a real caller of ProcessOrder (D2 ground truth).
package order

func HandleRequest(order map[string]string) map[string]string {
	_ = ValidateOrder(order)
	return ProcessOrder(order)
}
