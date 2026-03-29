# Block invocation patterns for relation extraction testing

class ItemProcessor
  def process_items(items)
    items.each { |item| puts item }
    items.map { |item| transform(item) }
    items.select { |item| valid?(item) }
  end

  def process_with_do_end(items)
    items.each do |item|
      validate(item)
      save(item)
    end
  end

  def nested_blocks(data)
    data.map do |row|
      row.select { |cell| filter(cell) }
    end
  end
end

class ChainedOperations
  def chain_example(collection)
    collection
      .map { |x| double(x) }
      .select { |x| positive?(x) }
      .reduce(0) { |sum, x| add(sum, x) }
  end
end
