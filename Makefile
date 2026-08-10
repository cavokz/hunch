DOCKER     := docker
ES_COMPOSE := tests/docker-compose-es.yml
ES_AUTH    := -u "$(ES_USER):$(ES_PASSWORD)"

lint:
	cargo fmt --all -- --check
	cargo clippy -q --all-targets -- -D warnings
	ruff check -q .

es-up:
	$(DOCKER) compose -f $(ES_COMPOSE) up -d

es-down:
	$(DOCKER) compose -f $(ES_COMPOSE) down -v

es-init:
	curl -sf $(ES_AUTH) "$(ES_URL)/_cluster/health?wait_for_status=yellow&timeout=120s" \
	  --retry 30 --retry-delay 5 --retry-all-errors | grep -q '"timed_out":false'
	curl -sf $(ES_AUTH) "$(ES_URL)/_index_template/foray" -X PUT \
	  -H 'Content-Type: application/json' -d @doc/es-index-template.json

es-logs:
	$(DOCKER) compose -f $(ES_COMPOSE) logs
