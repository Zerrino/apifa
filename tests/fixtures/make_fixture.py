#!/usr/bin/env python3
"""Writes tests/fixtures/apidoc.json: the docs of "Acme Shop", a made-up API, in the apipie format
(what Rails' apipie-rails exports). Run it again after changing it: python3 tests/fixtures/make_fixture.py
Nothing in it is real: names, ids and emails are invented."""
import json, os

def code_list(xs):
    return "Must be one of: " + ", ".join(f"<code>{x}</code>" for x in xs) + "."

def p(name, desc="", required=False, kind="String", full=None, children=None, allow_nil=False):
    v = {"String": "Must be String", "Integer": "Must be Integer", "Hash": "Must be a Hash", "DateTime": "Must be DateTime",
         "Array": "Must be an array of String", "IntArray": "Must be an array of Integer", "File": "Must be File", "Float": "Must be Float"}.get(kind, kind)
    out = {"name": name, "full_name": full or name, "description": f"\n<p>{desc}</p>\n" if desc else "", "required": required,
           "allow_nil": allow_nil, "validator": v, "expected_type": "hash" if kind == "Hash" else ("numeric" if kind == "Integer" else "string"),
           "metadata": None, "show": True, "validations": []}
    if children:
        out["params"] = children
    return out

def enum(name, options, desc="", required=False, full=None):
    return {"name": name, "full_name": full or name, "description": f"\n<p>{desc}</p>\n", "required": required, "allow_nil": False,
            "validator": code_list(options), "expected_type": "string", "metadata": None, "show": True, "validations": []}

def list_params(fields, ranges=None, ids=()):
    out = [p(i, f"The {i[:-3]} id or slug") for i in ids]
    out += [enum("sort", fields, "The sort field. Sorted by id desc by default."),
            enum("filter", fields, "Filtering on one or more fields"),
            enum("range", ranges or ["id", "created_at"], "Select on a particular range"),
            p("page", "The pagination", kind="Hash", children=[
                p("number", "The current page", kind="Integer", full="page[number]"),
                p("size", "The number of items per page, 1 to 100, default 30", kind="Integer", full="page[size]")])]
    return out

def api(url, verb="GET", deprecated=None, desc=None):
    return {"api_url": url, "http_method": verb, "short_description": desc, "deprecated": deprecated}

def method(name, apis, params=None, desc="", examples=None):
    m = {"doc_url": "", "name": name, "apis": apis, "formats": None, "full_description": f"<p>{desc}</p>" if desc else "", "errors": [], "params": params or []}
    if examples:
        m["examples"] = examples
    return m

def example(path, data, verb="GET", code="200", request=None):
    return {"verb": verb, "path": path, "versions": ["1.0"], "query": "", "request_data": request, "response_data": data, "code": code, "show_in_doc": 1, "recorded": True}

STORE = {"id": 3, "name": "Downtown", "slug": "downtown", "city": "Springfield", "created_at": "2025-03-01T09:00:00.000Z"}
CATEGORY = {"id": 5, "name": "Garden", "slug": "garden"}
USER = {"id": 101, "username": "jdoe", "email": "jdoe@example.com", "first_name": "Jane", "last_name": "Doe", "kind": "customer",
        "store_id": 3, "created_at": "2025-04-02T10:00:00.000Z", "avatar": {"url": "https://cdn.example.com/avatars/101.png"}}
PRODUCT = {"id": 7, "name": "Watering can", "slug": "watering-can", "price": 12.5, "category": CATEGORY, "store_ids": [3], "created_at": "2025-01-10T08:00:00.000Z"}
ORDER = {"id": 9001, "status": "shipped", "total": 25.0, "user": {"id": 101, "username": "jdoe"}, "product_ids": [7],
         "items": [{"id": 1, "product": {"id": 7, "name": "Watering can"}, "quantity": 2}], "created_at": "2025-05-05T12:00:00.000Z"}
TEAM = {"id": 12, "name": "Night shift", "store_id": 3, "users": [{"id": 101, "username": "jdoe"}]}

def crud(key, singular, fields, body, paths_index, examples_index=None, examples_show=None, show=True, write=True, nested_ids=()):
    ms = [method("index", [api(u) for u in paths_index], list_params(fields, ids=nested_ids), f"Return all the {key}", examples_index)]
    if show:
        ms.append(method("show", [api(f"/v1/{key}/:id")], [p("id", "The requested id", required=True)], f"Get a {singular}", examples_show))
    if write:
        ms.append(method("create", [api(f"/v1/{key}", "POST")], [p(singular, kind="Hash", children=body)], f"Create a {singular}"))
        ms.append(method("update", [api(f"/v1/{key}/:id", "PATCH"), api(f"/v1/{key}/:id", "PUT")], [p("id", "The requested id", required=True), p(singular, kind="Hash", children=body)], f"Update a {singular}"))
        ms.append(method("destroy", [api(f"/v1/{key}/:id", "DELETE")], [p("id", "The requested id", required=True)], f"Destroy a {singular}"))
    return ms

def fields(singular, names, kinds=None, required=()):
    kinds = kinds or {}
    return [p(n, f"The {n.replace('_', ' ')}.", n in required, kinds.get(n, "String"), full=f"{singular}[{n}]") for n in names]

resources = {
    "users": {"name": "Users", "short_description": "The people of the shop: customers and staff", "methods": crud(
        "users", "user", ["id", "username", "email", "kind", "store_id", "created_at"],
        fields("user", ["username", "email", "first_name", "last_name", "kind", "store_id", "avatar"], {"store_id": "Integer", "avatar": "File"}, required=("username", "email")),
        ["/v1/users", "/v1/stores/:store_id/users", "/v1/teams/:team_id/users"], [example("/v1/users", [USER])], [example("/v1/users/101", USER)],
        nested_ids=("store_id", "team_id"))},
    "stores": {"name": "Stores", "short_description": "Where things are sold", "methods": crud(
        "stores", "store", ["id", "name", "slug", "city", "created_at"], fields("store", ["name", "slug", "city"]),
        ["/v1/stores"], [example("/v1/stores", [STORE])], None)},
    "categories": {"name": "Categories", "short_description": "Groups of products", "methods": crud(
        "categories", "category", ["id", "name", "slug"], fields("category", ["name", "slug"]), ["/v1/categories"], [example("/v1/categories", [CATEGORY])], None, write=False)},
    "products": {"name": "Products", "short_description": "What a store sells", "methods": crud(
        "products", "product", ["id", "name", "slug", "category_id", "price", "created_at"],
        fields("product", ["name", "slug", "price", "category_id", "store_ids"], {"price": "Float", "category_id": "Integer", "store_ids": "IntArray"}, required=("name",)),
        ["/v1/products", "/v1/categories/:category_id/products", "/v1/stores/:store_id/products", "/v1/stores/:store_id/categories/:category_id/products"], [example("/v1/products", [PRODUCT])], None,
        nested_ids=("category_id", "store_id"))},
    "orders": {"name": "Orders", "short_description": "What users bought", "methods": crud(
        "orders", "order", ["id", "status", "user_id", "total", "created_at"],
        fields("order", ["user_id", "product_ids", "note", "status"], {"user_id": "Integer", "product_ids": "IntArray"}, required=("user_id",)),
        ["/v1/orders", "/v1/users/:user_id/orders"], [example("/v1/orders", [ORDER])], None, nested_ids=("user_id",))},
    "order_items": {"name": "Order items", "short_description": "The lines of an order", "methods": [
        method("index", [api("/v1/orders/:order_id/order_items")], [p("order_id", "The order id", required=True)] + list_params(["id", "product_id", "quantity"]), "The items of an order")]},
    "teams": {"name": "Teams", "short_description": "Staff working together", "methods": crud(
        "teams", "team", ["id", "name", "store_id"], fields("team", ["name", "store_id"], {"store_id": "Integer"}),
        ["/v1/teams", "/v1/stores/:store_id/teams"], [example("/v1/teams", [TEAM])], None, nested_ids=("store_id",))},
    "reviews": {"name": "Reviews", "short_description": "What users think of products", "methods": [
        method("index", [api("/v1/products/:product_id/reviews"), api("/v1/reviews")], list_params(["id", "rating", "user_id", "product_id"]) + [p("product_id", "The product id")], "Reviews"),
        method("create", [api("/v1/products/:product_id/reviews", "POST")], [p("product_id", "The product id", required=True), p("review", kind="Hash", children=fields("review", ["rating", "comment", "photo"], {"rating": "Integer", "photo": "File"}, required=("rating",)))], "Review a product")]},
    "events": {"name": "Events", "short_description": "Sales and store events", "methods": [
        method("index", [api("/v1/events"), api("/v1/stores/:store_id/events")], list_params(["id", "name", "begin_at", "end_at"], ["begin_at", "end_at"], ids=("store_id",)), "Events",
               [example("/v1/events", [{"id": 40, "name": "Spring sale", "begin_at": "2025-03-20T09:00:00.000Z", "end_at": None, "store_id": 3}])])]},
    "visits": {"name": "Visits", "short_description": "Who came to a store, when", "methods": [
        method("index", [api("/v1/stores/:store_id/visits")], [p("store_id", "The store id", required=True)] + list_params(["id", "user_id", "begin_at", "end_at"], ["begin_at", "end_at"]), "Visits of a store")]},
    "legacy_coupons": {"name": "Legacy coupons", "short_description": "Old coupons", "methods": [
        method("index", [api("/v1/coupons", deprecated=True)], [], "Use the promotions instead")]},
}
doc = {"docs": {"name": "Acme Shop API", "info": "A made-up shop API used by the tests.", "copyright": "", "doc_url": "/apidoc", "api_url": "", "resources": resources}}
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "apidoc.json")
with open(path, "w") as f:
    json.dump(doc, f, indent=1)
print(path)
