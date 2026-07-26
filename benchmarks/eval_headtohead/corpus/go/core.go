// Package order — pipeline core; the symbols the eval questions target.
package order

// OrderConfig configures the order pipeline (D4 target type).
type OrderConfig struct {
	Retries int
	Region  string
}

// LoadConfig returns the default OrderConfig (D1/D5 target).
func LoadConfig() OrderConfig {
	return OrderConfig{Retries: 3, Region: "eu"}
}

// ValidateOrder validates an order (D5 'validate' target).
func ValidateOrder(order map[string]string) error {
	if _, ok := order["id"]; !ok {
		return errMissingID
	}
	return nil
}

// ProcessOrder processes a single validated order (D1/D2 target symbol).
func ProcessOrder(order map[string]string) map[string]string {
	_ = ValidateOrder(order)
	return map[string]string{"id": order["id"], "status": "processed"}
}
