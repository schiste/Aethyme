const form = document.querySelector("#quantity-form");
const quantity = document.querySelector("#quantity");
const count = document.querySelector("#cart-count");
let cartItems = 0;

form.addEventListener("submit", (event) => {
  event.preventDefault();
  cartItems += Number(quantity.value);
  count.textContent = `Cart items: ${cartItems}`;
});
