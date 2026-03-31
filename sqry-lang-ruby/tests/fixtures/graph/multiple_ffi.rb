require "ffi"

# Test multiple FFI attach_function calls in same module
module CryptoLibrary
  extend FFI::Library

  # First FFI binding
  def self.setup_encryption
    FFI::Library.attach_function :aes_encrypt, [:pointer, :int, :pointer], :int
    FFI::Library.attach_function :aes_decrypt, [:pointer, :int, :pointer], :int
  end

  # Second set of FFI bindings
  def self.setup_hashing
    attach_function :sha256_hash, [:pointer, :int], :pointer
    attach_function :sha512_hash, [:pointer, :int], :pointer
  end

  # Mixed FFI and regular methods
  def self.process_data(data)
    encrypted = aes_encrypt(data, data.size, key_buffer)
    hashed = sha256_hash(encrypted, encrypted.size)
    hashed
  end

  # FFI with different receiver patterns
  def self.advanced_setup
    # Direct receiver
    FFI::Library.attach_function :random_bytes, [:int], :pointer

    # Via self (should still detect)
    self.attach_function :secure_random, [:int], :pointer

    # Via constant
    FFI.attach_function :system_entropy, [], :int
  end
end

# Another module with FFI
module CompressionLib
  extend FFI::Library

  def self.init
    FFI::Library.attach_function :compress_data, [:pointer, :size_t, :pointer], :int
    FFI::Library.attach_function :decompress_data, [:pointer, :size_t, :pointer], :int
  end
end
