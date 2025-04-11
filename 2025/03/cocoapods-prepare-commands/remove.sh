#!/usr/bin/env bash

set -euxo pipefail

pod_name=$1
shift

if [ -z "$pod_name" ]; then
  echo "Usage: $0 <pod_name>"
  exit 1
fi

while [ -n "$pod_name" ]; do
  versions_to_delete=$(cat podspecs_with_prepare_commands.json | jq --compact-output "[.podspecs[\"$pod_name\"] | .[] | .version]")

  command=$(
    cat <<RB
deletor =  Owner.find_by_email("segiddins@segiddins.me")
pod = Pod::TrunkApp::Pod.find_by_name("$pod_name")
versions_to_delete = $versions_to_delete
pod.versions.each do |v|
  next if v.deleted
  next unless versions_to_delete.include?(v.name)
  puts v.description
  pp v.delete!(deletor)
  sleep 0.5
end
RB
  )

  heroku run --app cocoapods-trunk-service -- ruby -I. -r bundler/setup -rirb -r config/init -e "${command}"

  pod_name=${1:-}
  shift || true
done

cargo run --release
