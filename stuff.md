## STUFF

# Bugs

- resources other than pods,deployments, secrets (the cached ones?) are very slow to list in the table
- the top bar kind selector must go, and the namespace one as well
- CRDs only showing 3 basic columns
- there's no indication when the result is empty, the spinner keeps spinning
- with a lot of logs the program becomes slow and then unresponsive

# Features

- clicking on a namespace in the results area should filter by namespace 
- logs should be colorized with tailspin crate
- in the results area, there should be a namespace dropdown selector, filterable and with multi checkbox for multiple namespaces
- add yaml syntax highlight to `egui_code_editor`
