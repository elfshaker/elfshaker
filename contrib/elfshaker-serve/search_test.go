package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"testing"
)

func search(t *testing.T, h *Handler, query string) (int, []Commit) {
	t.Helper()
	r := httptest.NewRequest(http.MethodGet, "/search/"+url.PathEscape(query), nil)
	w := httptest.NewRecorder()
	h.handleSearch(w, r)
	if w.Code != http.StatusOK {
		return w.Code, nil
	}
	if w.Header().Get("Content-Type") != "application/json" {
		t.Fatalf("content type %q", w.Header().Get("Content-Type"))
	}
	var results []Commit
	if err := json.Unmarshal(w.Body.Bytes(), &results); err != nil {
		t.Fatal(err)
	}
	if results == nil {
		t.Fatal("empty search results must be a JSON array")
	}
	return w.Code, results
}

func TestSearchRequiresAllTerms(t *testing.T) {
	h := &Handler{commits: Commits{{SHA: "old", Text: "old\x1fdate\x1falice\x1fcompiler"}, {SHA: "author", Text: "author alice"}, {SHA: "subject", Text: "subject compiler"}, {SHA: "new", Text: "new\x1fdate\x1falice\x1fcompiler"}}}
	status, results := search(t, h, "alice compiler")
	var shas []string
	for _, c := range results {
		shas = append(shas, c.SHA)
	}
	if status != http.StatusOK || !reflect.DeepEqual(shas, []string{"new", "old"}) {
		t.Fatalf("status %d, results %v", status, shas)
	}
}

func TestSearchLimitAndEmptyQueries(t *testing.T) {
	h := &Handler{}
	for i := 0; i < 12; i++ {
		h.commits = append(h.commits, Commit{SHA: fmt.Sprint(i), Text: "match"})
	}
	status, results := search(t, h, "match")
	if status != http.StatusOK || len(results) != 10 || results[0].SHA != "11" || results[9].SHA != "2" {
		t.Fatalf("status %d, results %+v", status, results)
	}
	status, results = search(t, h, "missing")
	if status != http.StatusOK || len(results) != 0 {
		t.Fatalf("status %d, results %+v", status, results)
	}
	for _, query := range []string{"", " \t "} {
		status, _ = search(t, h, query)
		if status != http.StatusBadRequest {
			t.Errorf("query %q: status %d", query, status)
		}
	}
}
