package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"unsafe"

	"helm.sh/helm/v3/pkg/action"
	"helm.sh/helm/v3/pkg/chart"
	"helm.sh/helm/v3/pkg/chart/loader"
	"helm.sh/helm/v3/pkg/chartutil"
	"helm.sh/helm/v3/pkg/cli"
)

type request struct {
	Name        string          `json:"name"`
	Chart       string          `json:"chart"`
	Namespace   *string         `json:"namespace"`
	APIVersions []string        `json:"apiVersions"`
	IncludeCRDs bool            `json:"includeCRDs"`
	NoHooks     bool            `json:"noHooks"`
	Values      json.RawMessage `json:"values"`
}

type response struct {
	YAML  string `json:"yaml,omitempty"`
	Error string `json:"error,omitempty"`
}

type cachedChart struct {
	digest [32]byte
	chart  *chart.Chart
}

var charts = struct {
	sync.Mutex
	byPath map[string]cachedChart
}{byPath: make(map[string]cachedChart)}

func chartDigest(path string) ([32]byte, error) {
	hash := sha256.New()
	err := filepath.WalkDir(path, func(name string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() {
			return nil
		}
		if !entry.Type().IsRegular() {
			return fmt.Errorf("non-regular chart entry: %s", name)
		}
		content, err := os.ReadFile(name)
		if err != nil {
			return err
		}
		hash.Write([]byte(name))
		hash.Write([]byte{0})
		hash.Write(content)
		hash.Write([]byte{0})
		return nil
	})
	var digest [32]byte
	if err == nil {
		copy(digest[:], hash.Sum(nil))
	}
	return digest, err
}

func loadChart(path string) (*chart.Chart, error) {
	digest, err := chartDigest(path)
	if err != nil {
		return loader.Load(path)
	}
	charts.Lock()
	defer charts.Unlock()
	if entry, ok := charts.byPath[path]; ok && entry.digest == digest {
		return entry.chart, nil
	}
	loaded, err := loader.Load(path)
	if err != nil {
		return nil, err
	}
	// Helm changes dependency metadata during rendering, so those charts must
	// get a fresh object for each call.
	if currentDigest, currentErr := chartDigest(path); currentErr == nil && currentDigest == digest && len(loaded.Dependencies()) == 0 && len(loaded.Metadata.Dependencies) == 0 {
		charts.byPath[path] = cachedChart{digest: digest, chart: loaded}
	} else {
		delete(charts.byPath, path)
	}
	return loaded, nil
}

func render(input []byte) (result response) {
	defer func() {
		if recovered := recover(); recovered != nil {
			result = response{Error: fmt.Sprintf("Helm panicked: %v", recovered)}
		}
	}()

	var args request
	if err := json.Unmarshal(input, &args); err != nil {
		return response{Error: err.Error()}
	}
	chart, err := loadChart(args.Chart)
	if err != nil {
		return response{Error: err.Error()}
	}
	if chart.Metadata.Type != "" && chart.Metadata.Type != "application" {
		return response{Error: fmt.Sprintf("%s charts are not installable", chart.Metadata.Type)}
	}
	if err := action.CheckDependencies(chart, chart.Metadata.Dependencies); err != nil {
		return response{Error: err.Error()}
	}
	values, err := chartutil.ReadValues(args.Values)
	if err != nil {
		return response{Error: err.Error()}
	}
	settings := cli.New()
	namespace := settings.Namespace()
	if args.Namespace != nil {
		namespace = *args.Namespace
	}
	config := &action.Configuration{Log: func(string, ...interface{}) {}}
	client := action.NewInstall(config)
	client.ClientOnly = true
	client.DryRun = true
	client.DryRunOption = "true"
	client.ReleaseName = args.Name
	client.Replace = true
	client.Namespace = namespace
	client.APIVersions = chartutil.VersionSet(args.APIVersions)
	client.IncludeCRDs = args.IncludeCRDs
	client.DisableHooks = args.NoHooks
	release, err := client.Run(chart, values)
	if err != nil {
		return response{Error: err.Error()}
	}
	var output strings.Builder
	output.WriteString(strings.TrimSpace(release.Manifest))
	output.WriteByte('\n')
	if !args.NoHooks {
		for _, hook := range release.Hooks {
			fmt.Fprintf(&output, "---\n# Source: %s\n%s\n", hook.Path, hook.Manifest)
		}
	}
	return response{YAML: output.String()}
}

//export HelmRender
func HelmRender(input *C.char, errorOut **C.char) *C.char {
	result := render([]byte(C.GoString(input)))
	if result.Error != "" {
		*errorOut = C.CString(result.Error)
		return nil
	}
	return C.CString(result.YAML)
}

//export HelmFree
func HelmFree(value *C.char) {
	C.free(unsafe.Pointer(value))
}

func main() {}
