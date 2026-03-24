# Test deeply nested namespaces to verify depth limiter
module Level1
  module Level2
    module Level3
      module Level4
        module Level5
          # At max depth (default is 4), this should be truncated
          module Level6
            class DeepClass
              def deep_method
                puts "very deep"
              end

              def self.deep_singleton
                "singleton at depth 6"
              end
            end
          end
        end

        # This should still be within depth
        class Level4Class
          def level4_method
            deep_call
          end
        end
      end
    end
  end
end

# Test that we can still extract from shallow levels
module Shallow
  class TestClass
    def test_method
      Level1::Level2::Level3::Level4Class.new.level4_method
    end
  end
end
