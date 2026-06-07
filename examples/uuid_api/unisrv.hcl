project = "uuid-api"

service "uuid" {
  hosts = ["uuid.unisrv.dev"]
}

deployment "uuid" {
  service = "uuid"
  port    = 8000

  build {
    context    = "."
    dockerfile = "Dockerfile"
  }
}
