class Level1 {
    class Level2 {
        class Level3 {
            class Level4 {
                func method1() {
                    // Should be extracted
                }
            }
        }
    }
}

class Level1Deep {
    class Level2Deep {
        class Level3Deep {
            class Level4Deep {
                class Level5Deep {
                    class Level6Deep {
                        func deepMethod() {
                            // May be truncated
                        }
                    }
                }
            }
        }
    }
}

class Shallow {
    func simpleMethod() {
        // Should always be extracted
    }
}
