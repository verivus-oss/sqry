require "ffi"

module Crypto
  def self.setup
    FFI::Library.attach_function :crypto_encrypt, [:pointer, :int], :int
  end

  def self.encrypt(buffer, size)
    crypto_encrypt(buffer, size)
  end
end
