project = "uuid-api"

service "uuid" {
  hosts = ["uuid.unisrv.dev"]

  location "/" {
    deployment = "uuid"
  }
}

deployment "uuid" {
  port = 8000

  build {
    context    = "."
    dockerfile = "Dockerfile"
  }
}
