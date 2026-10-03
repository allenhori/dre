select * from {{ source('sales', 'orders') }}
