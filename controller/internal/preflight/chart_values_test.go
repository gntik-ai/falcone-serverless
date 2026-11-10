package preflight_test

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
)

func TestParseValuesAcceptsPublishedChartValues(t *testing.T) {
	dir := "../../../docs/contracts/serverless-chart-values/fixtures/valid"
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) == 0 {
		t.Fatal("published valid fixture set is empty")
	}
	for _, entry := range entries {
		t.Run(entry.Name(), func(t *testing.T) {
			data, err := os.ReadFile(filepath.Join(dir, entry.Name()))
			if err != nil {
				t.Fatal(err)
			}
			if _, err := preflight.ParseValues(data); err != nil {
				t.Fatalf("published chart values rejected by preflight: %v", err)
			}
		})
	}
}
