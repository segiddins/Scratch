#!/usr/bin/env -S ruby --disable=gems
# frozen_string_literal: true

$:.unshift File.expand_path "~/Development/github.com/rubygems/rubygems/lib"

require 'rubygems'
require 'bundler/inline'

gemfile do
  source 'https://rubygems.org'
  gem 'prop_check'
end

PropCheck::Property.configure do |config|
  config.n_runs = 2000
  config.max_generate_attempts = 100_000
  config.max_consecutive_attempts = 100_000
end

G = PropCheck::Generators

def list_of(x)
  G.one_of(*x.map { |x| G.constant(x) })
end

known_cpus = list_of(%w[x86 x86_64 arm arm64 i386 i486 aarch64])
known_platforms = list_of(%w[linux darwin freebsd mingw mswin mswin64 java jruby aix cygwin macruby dalvik dotnet mingw mingw32 mswin openbsd solaris wasi test_platform])
version_like = list_of(%w[1 1.0 1..0 1.. .0 1. .. 12299 gnueabihf])
parts = G.one_of(G.constant(""), G.constant(0), known_cpus, known_platforms, version_like)

platform_strings = G.tree(parts) do |subtree_gen|
  G.array(subtree_gen, min: 0, max: 4).map(&:join)
end

PropCheck.forall(G.array(platform_strings, min: 0, max: 5).map { |it| it.join("-") }) do |platform_string|
  platform = begin
    Gem::Platform.new(platform_string)
  rescue ArgumentError => e
    next if e.message == "empty cpu in platform #{platform_string.inspect}"
    raise
  end
  platform2 = Gem::Platform.new(platform.to_s)

  raise <<~MSG unless platform == platform2
    From      #{platform_string.inspect}
    Expected: #{platform.inspect}
              #{platform}
    Got:      #{platform2.inspect}
              #{platform2}
  MSG
end
