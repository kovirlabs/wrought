# The two changes from the generated production config, both because the
# benchmark runs over plain HTTP on loopback like every other system:
# no forced TLS (the generator turns it on), and no host allow-list.
Rails.application.configure do
  config.force_ssl = false
  config.assume_ssl = false
  config.hosts.clear
end
