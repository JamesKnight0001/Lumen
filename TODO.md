# TODO

## High Priority

### HTTP/s Client
* [ ] Implement a HTTP/s client package, for lumen, THIS IS very important to make this good as possible.
  * Example:
    ```
    import http

    http.get(...)
    http.post(...)
    http.put(...)
    http.delete(...)
    ```
    Supporting HTTP/HTTPS requests (GET, POST, PUT, DELETE, etc.).

### Package Management System

* [ ] Design and implement a package management system for Lumen, similar to Python's package ecosystem.

  * Support package installation by name:

    ```bash
    lumen install <package>
    ```
  * Support package installation from remote sources:

    ```bash
    lumen install https://example.com/package
    ```
  * Evaluate and integrate a centralized package registry and/or CDN-backed distribution model.
  * Implement automatic dependency resolution and version management.
  * Provide a seamless and intuitive user experience with clear error reporting and documentation.
  * Why? Because to make it as userfriendly as possible, and easy to use.

### Virtual Environment Support

* [ ] Implement isolated project environments to ensure dependency separation and reproducible builds.

  * Create virtual environments:

    ```bash
    lumen venv ./venv
    ```
  * Allow packages to be installed into a specific environment.
  * Support environment activation and project-local dependency management.
  * Ensure compatibility with future package management features.
  * Why? Because why tf not?

### Linux Support

* [ ] Add official Linux support.

  * Validate compatibility across major Linux distributions.
  * Implement platform-specific tooling where necessary.
  * Establish automated testing and release pipelines for Linux targets.

## Low Priority

### Update system
* [ ] Implement a Update system, to update the compiler when needed, for example:
      ```bash
      lumen update
      ```

### Make more packages -> DO as Package Management system is created.
* [ ] Make more useful packages, to make it easier to make complex programs,
      for example, packages for easy UI creation via ``DirectX``.
      Or AI artitechture by porting ``torch`` to Lumen somehow.
      Or porting ``Pillow`` but for Lumen.