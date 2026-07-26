// Application entry point (D3 process root).
package order

func Main() map[string]string {
	ApplyConfig(map[string]int{"retries": 5})
	return HandleRequest(map[string]string{"id": "A-1"})
}
