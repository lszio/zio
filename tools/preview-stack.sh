#!/usr/bin/env bash
# Create, update or delete one pull request's Dokploy Compose stack + domain.
# The script never receives repository code: Dokploy builds the PR commit.
#
# Endpoint shapes VERIFIED against the live Dokploy API
# (https://dokploy.lszio.space, tRPC, header x-api-key, 2026-10-09):
#   POST compose.create            {projectId,environmentId,name,sourceType:"github",composeType:"docker-compose"} -> composeId
#                                  (create IGNORES git fields; set them via compose.update)
#   GET  environment.one           {environmentId} -> environment with a compose[] list (name -> composeId lookup;
#                                  compose.one only accepts a composeId and 404s on a name, VERIFIED 2026-10-09)
#   POST compose.update            {composeId,sourceType:"github",githubId,owner,repository,branch,composePath,autoDeploy}
#   POST compose.saveEnvironment   {composeId,env:"K=V\n..."}
#   POST compose.deploy            {composeId}
#   GET  domain.byComposeId        {composeId} -> list with domainId
#   POST domain.delete             {domainId}          (domain.deleteByComposeId DOES NOT EXIST: 404 NOT_FOUND)
#   POST compose.delete            {composeId,deleteVolumes:true}  (endpoint verified; deleteVolumes field assumed)
# ASSUMED (not verified against a live PR stack):
#   - Dokploy can clone the GitHub ref "pull/<N>/head" as a branch.
#   - compose.create response path .result.data.json.composeId.
# RESIDUAL RACE (compose.update API takes a branch/ref only, no commit SHA):
#   the deploy builds whatever `pull/<N>/head` points at when Dokploy clones,
#   which may have moved past the head SHA whose checks passed in CI. The
#   check-gate in preview.yml therefore bounds (but cannot eliminate) this
#   race; pinning the SHA requires a Dokploy create/update API change.
set -euo pipefail

dry_run=false
if [ "${1:-}" = "--dry-run" ]; then
  dry_run=true
  shift
fi

api="${DOKPLOY_API_URL:?DOKPLOY_API_URL is required, e.g. https://dokploy.lszio.space}"
token="${DOKPLOY_API_TOKEN:?DOKPLOY_API_TOKEN is required}"
project_id="${DOKPLOY_PROJECT_ID:?DOKPLOY_PROJECT_ID is required (Dokploy project for previews)}"
env_id="${DOKPLOY_ENVIRONMENT_ID:?DOKPLOY_ENVIRONMENT_ID is required (environment inside that project)}"
github_id="${DOKPLOY_GITHUB_ID:?DOKPLOY_GITHUB_ID is required (GitHub App provider id)}"
repo_owner="${GITHUB_REPO_OWNER:-lszio}"
repo_name="${GITHUB_REPO_NAME:-zio}"
compose_file="${PREVIEW_COMPOSE_FILE:-docker-compose.preview.yml}"
project="${PREVIEW_PROJECT:?PREVIEW_PROJECT is required, e.g. zio-pr-123}"
domain="${PREVIEW_DOMAIN:?PREVIEW_DOMAIN is required, e.g. zio-pr-123.lszio.space}"
pr_number="${PR_NUMBER:?PR_NUMBER is required}"

post() { # post <procedure> <json-body>
  if $dry_run; then
    printf 'POST %s/api/trpc/%s\n  %s\n' "$api" "$1" "$2" >&2
  else
    curl -sS --fail-with-body -X POST "$api/api/trpc/$1" \
      -H "x-api-key: $token" -H 'Content-Type: application/json' -d "{\"json\":$2}"
    printf '\n'
  fi
}

get() { # get <procedure> <json-input>
  if $dry_run; then
    printf 'GET %s/api/trpc/%s?input=%s\n' "$api" "$1" "$(jq -rn --arg v "$2" '$v|@uri')" >&2
  else
    # Propagate curl's status: a trailing printf would turn a 401/transport
    # failure into success and make the name lookup report "absent".
    local body status=0
    body="$(curl -sS --fail-with-body "$api/api/trpc/$1?input=$(jq -rn --arg v "$2" '$v|@uri')" \
      -H "x-api-key: $token")" || status=$?
    printf '%s\n' "$body"
    return "$status"
  fi
}

# composeId of the PR stack, or empty if absent. Stacks are resolved by
# NAME within the environment: the PR name is not a composeId, and using
# it as one made `synchronize` create a duplicate stack and PR close delete
# nothing.
resolve_compose_id() {
  if $dry_run; then
    get environment.one "{\"environmentId\":\"$env_id\"}" # name -> composeId lookup
    printf '<composeId-of-%s>\n' "$project"
  else
    # No `|| true`: with `set -euo pipefail` a transport/auth/JSON failure
    # propagates and aborts, while a successful lookup without a match
    # still means absent (empty composeId).
    get environment.one "{\"environmentId\":\"$env_id\"}" \
      | jq -r --arg name "$project" '(.result.data.json // .result.data // .).compose[]? | select(.name == $name) | .composeId'
  fi
}

case "${1:?usage: preview-stack.sh [--dry-run] up|delete}" in
  up)
    compose_id="$(resolve_compose_id)"
    if [ -n "$compose_id" ]; then
      printf 'stack %s exists (%s), updating\n' "$project" "$compose_id"
    else
      compose_id="$(post compose.create "{\"projectId\":\"$project_id\",\"environmentId\":\"$env_id\",\"name\":\"$project\",\"sourceType\":\"github\",\"composeType\":\"docker-compose\"}" \
        | jq -r '.result.data.json.composeId // .result.data.composeId // .composeId')"
      [ -n "$compose_id" ] || { echo 'compose.create returned no composeId' >&2; exit 1; }
      printf 'created stack %s (%s)\n' "$project" "$compose_id"
    fi
    post compose.update "{\"composeId\":\"$compose_id\",\"sourceType\":\"github\",\"githubId\":\"$github_id\",\"owner\":\"$repo_owner\",\"repository\":\"$repo_name\",\"branch\":\"pull/$pr_number/head\",\"composePath\":\"$compose_file\",\"autoDeploy\":true}" >/dev/null
    preview_env=""
    for v in "${!GROVE_PREVIEW_TOKEN_@}"; do
      preview_env+="${v}=${!v}"$'\n'
    done
    post compose.saveEnvironment "{\"composeId\":\"$compose_id\",\"env\":$(jq -rn --arg v "$preview_env" '$v|tojson')}" >/dev/null
    post compose.deploy "{\"composeId\":\"$compose_id\",\"title\":\"pr-$pr_number\",\"description\":\"preview for PR #$pr_number\"}" >/dev/null
    # Replace any stale domain pointing at this stack, then create the current one.
    if ! $dry_run; then
      get domain.byComposeId "{\"composeId\":\"$compose_id\"}" \
        | jq -r '(.result.data.json // .result.data // .)[]?.domainId' \
        | while IFS= read -r d; do post domain.delete "{\"domainId\":\"$d\"}" >/dev/null; done
    fi
    post domain.create "{\"host\":\"$domain\",\"path\":\"/\",\"port\":80,\"https\":true,\"certificateType\":\"letsencrypt\",\"composeId\":\"$compose_id\",\"serviceName\":\"site\",\"domainType\":\"compose\"}" >/dev/null
    printf 'preview %s is deploying from pull/%s/head\n' "$domain" "$pr_number"
    ;;
  delete)
    compose_id="$(resolve_compose_id)"
    if [ -z "$compose_id" ] && ! $dry_run; then
      printf 'no stack named %s; nothing to delete\n' "$project"
      exit 0
    fi
    if ! $dry_run; then
      ids="$(get domain.byComposeId "{\"composeId\":\"$compose_id\"}" \
        | jq -r '(.result.data.json // .result.data // .)[]?.domainId' || true)"
      for d in $ids; do post domain.delete "{\"domainId\":\"$d\"}" >/dev/null || true; done
    else
      post domain.byComposeId "{\"composeId\":\"$compose_id\"}" >/dev/null # dry-run: show the lookup
      post domain.delete '{"domainId":"<from lookup>"}' >/dev/null
    fi
    post compose.delete "{\"composeId\":\"$compose_id\",\"deleteVolumes\":true}" >/dev/null
    printf 'preview stack %s deleted with its volumes\n' "$compose_id"
    ;;
  *)
    printf 'unknown action %s\n' "$1" >&2
    exit 2
    ;;
esac
