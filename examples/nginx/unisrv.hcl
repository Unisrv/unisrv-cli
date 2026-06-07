project = "nginx"

service "nginx" {}

deployment "nginx" {
  service = "nginx"
  port    = 80
  container {
    image = "nginx:latest"
  }
}
