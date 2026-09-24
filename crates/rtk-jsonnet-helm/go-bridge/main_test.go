package main

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"helm.sh/helm/v3/pkg/chart/loader"
)

func TestChartCacheInvalidatesOnContentChange(t *testing.T) {
	path := t.TempDir()
	if err := os.WriteFile(filepath.Join(path, "Chart.yaml"), []byte("apiVersion: v2\nname: test\nversion: 0.1.0\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(path, "templates"), 0700); err != nil {
		t.Fatal(err)
	}
	template := filepath.Join(path, "templates", "configmap.yaml")
	if err := os.WriteFile(template, []byte("kind: ConfigMap\nmetadata:\n  name: first\n"), 0600); err != nil {
		t.Fatal(err)
	}
	first, err := loadChart(path)
	if err != nil {
		t.Fatal(err)
	}
	second, err := loadChart(path)
	if err != nil || first != second {
		t.Fatalf("unchanged chart was not reused: %v", err)
	}
	if err := os.WriteFile(template, []byte("kind: ConfigMap\nmetadata:\n  name: other\n"), 0600); err != nil {
		t.Fatal(err)
	}
	third, err := loadChart(path)
	if err != nil || third == second {
		t.Fatalf("changed chart was reused: %v", err)
	}
}

func BenchmarkLinkedRender(b *testing.B) {
	path, err := filepath.Abs("../../../rtk-benchmarks/helm-template/charts/bench-chart")
	if err != nil {
		b.Fatal(err)
	}
	input := []byte(`{"name":"bench","chart":"` + path + `","namespace":"bench","includeCRDs":true,"values":{"replicaCount":2}}`)
	b.ResetTimer()
	for range b.N {
		if result := render(input); result.Error != "" {
			b.Fatal(result.Error)
		}
	}
}

func BenchmarkChartLoad(b *testing.B) {
	path, err := filepath.Abs("../../../rtk-benchmarks/helm-template/charts/bench-chart")
	if err != nil {
		b.Fatal(err)
	}
	b.ResetTimer()
	for range b.N {
		if _, err := loader.Load(path); err != nil {
			b.Fatal(err)
		}
	}
}

func TestParallelRender(t *testing.T) {
	path, err := filepath.Abs("../../../rtk-benchmarks/helm-template/charts/bench-chart")
	if err != nil {
		t.Fatal(err)
	}
	input := []byte(`{"name":"bench","chart":"` + path + `","namespace":"bench","includeCRDs":true,"values":{"replicaCount":2}}`)
	first := render(input)
	if first.Error != "" || !strings.Contains(first.YAML, "kind: Deployment") {
		t.Fatalf("first render failed: %s", first.Error)
	}
	var workers sync.WaitGroup
	for range 8 {
		workers.Add(1)
		go func() {
			defer workers.Done()
			result := render(input)
			if result.Error != "" || result.YAML != first.YAML {
				t.Errorf("parallel render differs: %s", result.Error)
			}
		}()
	}
	workers.Wait()
}
