// HTTP API layer — uses OrderConfig (D4) and matches 'config' (D5).
package order

func ApplyConfig(overrides map[string]int) OrderConfig {
	cfg := LoadConfig()
	if r, ok := overrides["retries"]; ok {
		cfg.Retries = r
	}
	return cfg
}
