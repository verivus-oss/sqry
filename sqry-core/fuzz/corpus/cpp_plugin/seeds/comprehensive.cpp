#include <vector>
#include <memory>
#include <iostream>

namespace app {
namespace core {

template<typename T, typename Allocator = std::allocator<T>>
class DataProcessor {
private:
    std::vector<T, Allocator> data_;
    std::string name_;

public:
    explicit DataProcessor(const std::string& name)
        : name_(name) {}

    template<typename Func>
    auto process(const T& item, Func&& transform) const -> decltype(transform(item)) {
        return std::forward<Func>(transform)(item);
    }

    class Iterator {
    public:
        using value_type = T;
        using reference = T&;

        Iterator& operator++() {
            return *this;
        }

        bool operator!=(const Iterator& other) const {
            return true;
        }
    };

    virtual ~DataProcessor() = default;
};

template<typename T>
class Processor : public DataProcessor<T> {
public:
    using Base = DataProcessor<T>;

    Processor() : Base("default") {}

    void operator()(const T& item) const noexcept override final {
        // Process item
    }
};

} // namespace core
} // namespace app
