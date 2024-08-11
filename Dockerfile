FROM cgr.dev/chainguard/static
COPY --chown=nonroot:nonroot ./controller /app/
EXPOSE 8080
ENTRYPOINT ["/app/controller"]

LABEL org.opencontainers.image.title=db-operator
LABEL org.opencontainers.image.description="Your friendly little neighbourhood PostgreSQL database operator."
LABEL org.opencontainers.image.base.name="ghcr.io/sunsided/db-operator:latest"
LABEL org.opencontainers.image.source=https://github.com/sunsided/db-operator
LABEL org.opencontainers.image.url=https://github.com/sunsided/db-operator
LABEL org.opencontainers.image.documentation=https://github.com/sunsided/db-operator
