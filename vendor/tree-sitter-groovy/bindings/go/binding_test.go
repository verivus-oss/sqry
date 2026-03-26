package tree_sitter_groovy_test

import (
	"testing"

	tree_sitter "github.com/smacker/go-tree-sitter"
	"github.com/tree-sitter/tree-sitter-groovy"
)

func TestCanLoadGrammar(t *testing.T) {
	language := tree_sitter.NewLanguage(tree_sitter_groovy.Language())
	if language == nil {
		t.Errorf("Error loading Groovy grammar")
	}
}
