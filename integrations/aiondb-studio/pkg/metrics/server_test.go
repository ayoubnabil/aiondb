package metrics

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestValidateServerPath(t *testing.T) {
	tests := []struct {
		name string
		path string
		err  string
	}{
		{name: "valid", path: "/metrics"},
		{name: "empty", path: "", err: "metrics path must not be empty"},
		{name: "relative", path: "metrics", err: "metrics path must start with /"},
		{name: "query delimiter", path: "/metrics?token=secret", err: "metrics path must not contain query or fragment delimiters"},
		{name: "fragment delimiter", path: "/metrics#secret", err: "metrics path must not contain query or fragment delimiters"},
		{name: "oversized", path: "/" + strings.Repeat("m", maxMetricsPathBytes+1), err: "metrics path must be less than or equal to 1024 bytes"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := validateServerPath(test.path)
			if test.err == "" {
				assert.NoError(t, err)
				return
			}
			assert.EqualError(t, err, test.err)
		})
	}
}
