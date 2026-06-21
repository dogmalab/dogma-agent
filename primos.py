def primeros_10_primos():
    """
    Devuelve una lista con los primeros 10 números primos.
    Un número primo es aquel mayor que 1 que solo es divisible por sí mismo y por 1.
    """
    primos = []
    numero = 2  # El primer número primo es 2

    while len(primos) < 10:
        es_primo = True
        # Solo verificamos divisores hasta la raíz cuadrada del número
        for divisor in range(2, int(numero ** 0.5) + 1):
            if numero % divisor == 0:
                es_primo = False
                break
        if es_primo:
            primos.append(numero)
        numero += 1

    return primos

# Ejemplo de uso
if __name__ == "__main__":
    print(primeros_10_primos())
