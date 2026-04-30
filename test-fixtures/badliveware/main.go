   package main

   type SelectorSource struct {
      NeedTags bool
      Other    bool
   }

   func parseConfig(input string) (bool, error) {
      return input != "", nil
   }

   func useSelector(selector SelectorSource) bool {
      ok, err := parseConfig("x")
      if err != nil {
         return false
      }
      if selector.NeedTags {
         return ok
      }
      selector.Other = false
      return selector.NeedTags
   }

   func unrelated() {
      NeedTags := "local variable"
      _ = NeedTags
   }
