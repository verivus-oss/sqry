class AccessorLocals {
    var backing: Int = 0

    var computed: Int
        get() {
            val temp = backing
            return temp
        }
        set(newValue) {
            val validated = newValue
            backing = validated
        }
}
