package client

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestTableRowsRejectsUnsafeBrowseOptions(t *testing.T) {
	client := &Client{}

	_, err := client.TableRows("users", RowsOptions{Where: "1=1; DROP TABLE users"})
	assert.Error(t, err)

	_, err = client.TableRows("users", RowsOptions{Where: "1=1 -- comment"})
	assert.Error(t, err)

	_, err = client.TableRows("users", RowsOptions{SortOrder: "ASC; DROP TABLE users"})
	assert.Error(t, err)

	_, err = client.TableRowsCount("users", RowsOptions{Where: "1=1; DROP TABLE users"})
	assert.Error(t, err)
}
